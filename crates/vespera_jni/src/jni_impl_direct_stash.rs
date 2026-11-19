//! DIRECT overflow stash: keep a response that overflowed the caller's
//! direct buffer so Java can fetch (or stream) it **without re-running the
//! handler**.
//!
//! The natives here are bound to the package-private Java class
//! `com.devfive.vespera.bridge.DirectStash` (new symbols — the existing
//! `VesperaBridge.dispatchDirect0` keeps its exact-size, discard-on-overflow
//! contract).
//!
//! # `dispatchResumable0` sentinel protocol
//!
//! Same `jint` protocol as `dispatchDirect0` — `>= 0` complete,
//! `Integer.MIN_VALUE` unrepresentable — except for an overflow: it returns
//! `-(required)` (clamped to `i32::MAX`; exact only when flagged, never
//! flagged when clamped) and writes
//! the stash handle into `out[0..9]` (an `out` below [`STASH_HANDLE_LEN`]
//! bytes is answered `-(MIN_DIRECT_OUT_BYTES)` without dispatching; a
//! handler panic's `500` wire is stashed like any response, so the handler
//! never re-runs): an 8-byte big-endian token (never `0`) followed by a
//! flags byte ([`FLAG_EXACT`], [`FLAG_STREAM`]).  The token is redeemed with
//! `fetch0` (copy into a larger buffer), `stream0` (header-first streaming)
//! or released with `discard0`.  An unredeemed entry expires after
//! [`STASH_TTL`] (swept on every stash access — put, fetch, stream,
//! discard); at most [`MAX_STASHED`] entries are kept (least recently
//! stashed evicted), and their held bytes stay within [`MAX_STASHED_BYTES`]
//! except for one response larger than that budget, which is kept alone
//! (evicting it would make the handler run again).
//! The byte count covers what an entry holds in memory (its header and the
//! body bytes already read), not the unread rest of a body.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use jni::EnvUnowned;
use jni::objects::{JByteBuffer, JClass};
use jni::sys::{jint, jlong};
use vespera_inprocess::{PendingDirectResponse, ResumableDirectWrite};

use super::{
    DIRECT_UNREPRESENTABLE, block_on_shared_runtime, direct_complete_code, direct_overflow_code,
    run_direct_symbol,
};
use crate::jni_impl::{
    guard_env_void_symbol, guard_void_symbol, panic_wire, runtime_unavailable_wire,
};

/// Bytes of the stash handle written into `out` on overflow.
pub const STASH_HANDLE_LEN: usize = 9;
/// Flag: the reported required size is exact.
pub const FLAG_EXACT: u8 = 1;
/// Flag: the response is an event stream — deliver it with `stream0`.
pub const FLAG_STREAM: u8 = 2;
/// How long an unredeemed stash entry is kept.
pub const STASH_TTL: Duration = Duration::from_secs(30);
/// Maximum number of stashed responses (oldest evicted first).
pub const MAX_STASHED: usize = 1024;
/// Maximum bytes the stash holds across its entries (see
/// [`PendingDirectResponse::held_bytes`]), oldest evicted first.  Matches the
/// default buffered-response cap.  A single response larger than this is
/// kept alone: refusing it would force the handler to run again.
pub const MAX_STASHED_BYTES: usize = 256 * 1024 * 1024;

/// One stashed response: when it was stashed, the response, and the bytes
/// it was counted at (what [`Stash::held`] added for it).
type Entry = (Instant, PendingDirectResponse, usize);

struct Stash {
    next: u64,
    /// Token → entry.  A partially fetched response is re-stashed under its
    /// old token with a fresh timestamp, so key order is NOT timestamp
    /// order: expiry and eviction compare timestamps.
    entries: BTreeMap<u64, Entry>,
    /// Sum of the byte counts of `entries`.
    held: usize,
}

impl Stash {
    /// Insert `pending` under `token`, counting its held bytes.
    fn insert(&mut self, token: u64, at: Instant, pending: PendingDirectResponse) {
        let bytes = pending.held_bytes();
        self.held += bytes;
        if let Some((_, _, replaced)) = self.entries.insert(token, (at, pending, bytes)) {
            self.held -= replaced;
        }
    }

    /// Remove `token`'s entry, releasing its counted bytes.
    fn remove(&mut self, token: u64) -> Option<(Instant, PendingDirectResponse)> {
        let (at, pending, bytes) = self.entries.remove(&token)?;
        self.held -= bytes;
        Some((at, pending))
    }

    /// Move every entry older than [`STASH_TTL`] into `evicted`, then — when
    /// `room` is `Some((incoming, budget))` — the least recently stashed
    /// ones until one more entry of `incoming` bytes fits both
    /// [`MAX_STASHED`] and `budget`.
    fn evict(
        &mut self,
        now: Instant,
        room: Option<(usize, usize)>,
        evicted: &mut Vec<(Instant, PendingDirectResponse)>,
    ) {
        let expired: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, (at, _, _))| now.duration_since(*at) > STASH_TTL)
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            evicted.extend(self.remove(token));
        }
        let Some((incoming, budget)) = room else {
            return;
        };
        while self.entries.len() >= MAX_STASHED || self.held.saturating_add(incoming) > budget {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (at, _, _))| *at)
                .map(|(token, _)| *token)
            else {
                break;
            };
            evicted.extend(self.remove(oldest));
        }
    }
}

static STASH: Mutex<Stash> = Mutex::new(Stash {
    next: 1,
    entries: BTreeMap::new(),
    held: 0,
});

fn lock_stash() -> std::sync::MutexGuard<'static, Stash> {
    STASH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Store `pending` under `token` (`None` = a fresh token), evicting expired
/// and surplus entries.  Evicted responses are dropped AFTER the lock is
/// released (dropping a body may run arbitrary code).
fn stash_put(token: Option<u64>, pending: PendingDirectResponse) -> u64 {
    stash_put_within(token, pending, MAX_STASHED_BYTES)
}

/// [`stash_put`] with an explicit byte budget (tests use a small one).
fn stash_put_within(token: Option<u64>, pending: PendingDirectResponse, budget: usize) -> u64 {
    let now = Instant::now();
    let mut evicted = Vec::new();
    let token = {
        let mut stash = lock_stash();
        let incoming = pending.held_bytes();
        stash.evict(now, Some((incoming, budget)), &mut evicted);
        let token = token.unwrap_or_else(|| {
            let token = stash.next;
            stash.next = stash.next.wrapping_add(1).max(1);
            token
        });
        stash.insert(token, now, pending);
        token
    };
    drop(evicted);
    token
}

/// Remove `token`'s entry (if still live) and sweep expired ones, so an
/// abandoned response (and the body / connection it holds) never outlives
/// [`STASH_TTL`] past the next stash access.  Dropped after the lock.
fn stash_take(token: u64) -> Option<PendingDirectResponse> {
    let mut evicted = Vec::new();
    let removed = {
        let mut stash = lock_stash();
        stash.evict(Instant::now(), None, &mut evicted);
        stash.remove(token)
    };
    drop(evicted);
    removed.map(|(_, pending)| pending)
}

/// Take a stashed response for streaming (`stream0`).
pub fn take_stashed(token: jlong) -> Option<PendingDirectResponse> {
    u64::try_from(token).ok().and_then(stash_take)
}

/// Drop every stashed response (runtime shutdown).
pub fn clear_stash() {
    let drained = {
        let mut stash = lock_stash();
        stash.held = 0;
        std::mem::take(&mut stash.entries)
    };
    drop(drained);
}

/// Number of live stash entries (tests).
#[cfg(test)]
pub fn stashed_count() -> usize {
    lock_stash().entries.len()
}

/// Bytes the live stash entries count (tests).
#[cfg(test)]
pub fn stashed_bytes() -> usize {
    lock_stash().held
}

/// `-(required)`, clamped so it is never the `MIN` sentinel.
fn pending_code(pending: &PendingDirectResponse) -> jint {
    -jint::try_from(pending.required_bytes()).unwrap_or(jint::MAX)
}

fn pending_flags(pending: &PendingDirectResponse) -> u8 {
    handle_flags(
        pending.required_bytes(),
        pending.is_exact(),
        pending.prefers_streaming(),
    )
}

/// Handle flags byte.  [`FLAG_EXACT`] is set only when the size
/// [`pending_code`] reports is the exact one: a size above `i32::MAX` is
/// clamped there, so it is never flagged exact.
fn handle_flags(required: usize, exact: bool, prefers_streaming: bool) -> u8 {
    let mut flags = 0;
    if exact && jint::try_from(required).is_ok() {
        flags |= FLAG_EXACT;
    }
    if prefers_streaming {
        flags |= FLAG_STREAM;
    }
    flags
}

/// Stash an overflowing response and write its handle into `out`.
///
/// Called while the dispatch still holds its runtime lease (inside the
/// `block_on` future), so a shutdown drains it before its `clear_stash`
/// runs; a shutdown already past that drain (its timeout expired) is caught
/// by the re-check below — the entry is taken back and the retryable `503`
/// every other request gets is answered instead of a handle to a response
/// nothing would ever release.
pub fn stash_overflow(out: &mut [u8], pending: PendingDirectResponse) -> jint {
    let code = pending_code(&pending);
    let flags = pending_flags(&pending);
    if out.len() < STASH_HANDLE_LEN {
        // No room for a handle: the response cannot be redeemed, drop it.
        return code;
    }
    let token = stash_put(None, pending);
    if vespera_inprocess::runtime::runtime_shut_down() {
        drop(stash_take(token));
        return write_error_wire(out, &runtime_unavailable_wire());
    }
    out[..8].copy_from_slice(&token.to_be_bytes());
    out[8] = flags;
    code
}

/// `com.devfive.vespera.bridge.DirectStash.dispatchResumable0(ByteBuffer, int, ByteBuffer) -> int`
///
/// Resumable sibling of `VesperaBridge.dispatchDirect0` (same buffer
/// contract and safety invariants, same caller-side validation): a
/// response that overflows `out` is stashed instead of discarded — see the
/// module docs for the handle protocol.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_DirectStash_dispatchResumable0<'local>(
    unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    in_buf: JByteBuffer<'local>,
    in_len: jint,
    out_buf: JByteBuffer<'local>,
) -> jint {
    run_direct_symbol(unowned_env, &in_buf, in_len, &out_buf, dispatch_resumable)
}

/// The JVM-free body of `dispatchResumable0`: `None` when the runtime is
/// unavailable (answered by `run_direct_symbol`).
///
/// No 1 KiB minimum here (unlike `dispatchDirect0`): any `out` that holds
/// a handle ([`STASH_HANDLE_LEN`] bytes) dispatches, and every overflow of
/// a response produced after the handler ran — its own response or the
/// `500` panic wire of a panicking handler — is stashed, so the handler
/// never re-runs.  A smaller `out` asks for
/// [`vespera_inprocess::MIN_DIRECT_OUT_BYTES`] without dispatching (as in
/// `dispatchDirect0`, the input checks of `run_direct_symbol` run first).  An
/// error wire that overflows without a handle zeroes the handle area
/// (`write_error_to_out` / [`write_error_wire`]), so no stale token is ever
/// read back; those are answered before any handler ran (or after a
/// shutdown, when a retry runs no handler either).
fn dispatch_resumable(input: &[u8], out: &mut [u8]) -> Option<jint> {
    if out.len() < STASH_HANDLE_LEN {
        return Some(direct_overflow_code(
            vespera_inprocess::MIN_DIRECT_OUT_BYTES,
        ));
    }
    // Stashed inside the future: under the dispatch's runtime lease.
    block_on_shared_runtime(async {
        let dispatched = AssertUnwindSafe(
            vespera_inprocess::dispatch_into_async_borrowed_resumable(input, &mut *out),
        )
        .catch_unwind()
        .await;
        // A handler panic: the panic wire takes the overflow path too.
        let written = dispatched
            .unwrap_or_else(|_| vespera_inprocess::resumable_from_wire(out, panic_wire()));
        match written {
            ResumableDirectWrite::Complete(n) => direct_complete_code(n),
            ResumableDirectWrite::Overflow(pending) => stash_overflow(out, pending),
        }
    })
}

/// `com.devfive.vespera.bridge.DirectStash.fetch0(long, ByteBuffer) -> int`
///
/// Continue the stashed response `token` into the direct buffer `out`:
/// `>= 0` complete (the entry is consumed), `-(required)` still too large
/// (the entry is kept under the same token, with more of the body read),
/// `Integer.MIN_VALUE` when `out` is not a direct buffer or, while the
/// runtime is live, the token is unknown (expired / evicted / already
/// redeemed).  After runtime shutdown an unknown token instead answers the
/// runtime-unavailable `503` wire, written into `out` like any other
/// response: `>= 0` its length, or `-(len)` when it does not fit (the handle
/// area is then zeroed, so the caller retries with a larger buffer).  A
/// configured dispatch timeout bounds the call (`504` wire).
///
/// A Rust panic while the stashed body is read (the handler's body stream
/// panicked) is never reported as `Integer.MIN_VALUE` — that would read as
/// "expired": it answers the `500` panic wire (`"panicked":"true"`) when it
/// fits in `out` (it always does in practice), and otherwise — or for a
/// panic outside that guard — throws `RuntimeException`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_DirectStash_fetch0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    token: jlong,
    out_buf: JByteBuffer<'local>,
) -> jint {
    let mut code = DIRECT_UNREPRESENTABLE;
    let mut fetch_panicked = false;
    let panicked = guard_env_void_symbol(&mut unowned_env, |env| {
        if out_buf.is_null() {
            return Ok(());
        }
        let out_addr = env.get_direct_buffer_address(&out_buf)?;
        let out_cap = env.get_direct_buffer_capacity(&out_buf)?;
        // SAFETY: `out_buf` is a live direct buffer (pinned by its local
        // ref for the whole call) covering `out_cap` writable bytes —
        // the Java caller only passes its own pooled direct buffers.
        // The slice never escapes this synchronous `block_on`.
        let out = unsafe { std::slice::from_raw_parts_mut(out_addr, out_cap) };
        let fetched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fetch_into(token, &mut *out)
        }));
        fetch_panicked = fetched.is_err();
        code = fetched.unwrap_or_else(|_| write_complete_wire(out, &panic_wire()));
        Ok(())
    });
    // A panic outside the fetch guard, or a panic wire that did not fit.
    if panicked || (fetch_panicked && code == DIRECT_UNREPRESENTABLE) {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            let _ = env.throw_new(
                jni::jni_str!("java/lang/RuntimeException"),
                jni::jni_str!("panic in Rust engine while fetching a stashed DIRECT response"),
            );
            Ok(())
        });
    }
    code
}

/// Redeem stash entry `token` into `out` (the body of `fetch0`, run under
/// its panic guard).
pub fn fetch_into(token: jlong, out: &mut [u8]) -> jint {
    let Some(pending) = take_stashed(token) else {
        // `shutdown_runtime` clears the stash once it has drained the
        // leases — possibly between `dispatchResumable0` and this fetch:
        // answer the retryable `503` every other request gets, not "expired".
        if vespera_inprocess::runtime::runtime_shut_down() {
            return write_error_wire(out, &runtime_unavailable_wire());
        }
        return DIRECT_UNREPRESENTABLE;
    };
    // Re-stashed inside the future, under the fetch's runtime lease (see
    // `stash_overflow`), with the same post-insert shutdown re-check.
    let fetched = block_on_shared_runtime(async {
        match pending.write_into(&mut *out).await {
            ResumableDirectWrite::Complete(n) => Some(direct_complete_code(n)),
            ResumableDirectWrite::Overflow(pending) => {
                let code = pending_code(&pending);
                let key = u64::try_from(token).unwrap_or_default();
                stash_put(Some(key), pending);
                if vespera_inprocess::runtime::runtime_shut_down() {
                    drop(stash_take(key));
                    return None;
                }
                Some(code)
            }
        }
    });
    // No runtime (shut down, before or during the fetch): the stashed
    // response is dropped; answer with the runtime-unavailable wire when it
    // fits.
    fetched
        .flatten()
        .unwrap_or_else(|| write_error_wire(out, &runtime_unavailable_wire()))
}

/// Copy a complete wire response into `out`: its length, or
/// `DIRECT_UNREPRESENTABLE` when it does not fit.
fn write_complete_wire(out: &mut [u8], wire: &[u8]) -> jint {
    out.get_mut(..wire.len())
        .map_or(DIRECT_UNREPRESENTABLE, |dst| {
            dst.copy_from_slice(wire);
            direct_complete_code(wire.len())
        })
}

/// Copy a complete error wire into `out` (its length), or — when it does
/// not fit — `-(len)` with the handle area zeroed: an overflow without a
/// handle, which the caller retries with a larger buffer (not
/// `Integer.MIN_VALUE`, which reads as "larger than 2 GiB").
fn write_error_wire(out: &mut [u8], wire: &[u8]) -> jint {
    if let Some(dst) = out.get_mut(..wire.len()) {
        dst.copy_from_slice(wire);
        return direct_complete_code(wire.len());
    }
    let handle = out.len().min(STASH_HANDLE_LEN);
    out[..handle].fill(0);
    direct_overflow_code(wire.len())
}

/// `com.devfive.vespera.bridge.DirectStash.discard0(long) -> void`
///
/// Drop the stashed response `token` (no-op when unknown).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_DirectStash_discard0<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    token: jlong,
) {
    let _ = guard_void_symbol(|| drop(take_stashed(token)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A materialised `400` (malformed wire, no app needed) that overflows
    /// an empty buffer.
    fn pending() -> PendingDirectResponse {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        match rt.block_on(vespera_inprocess::dispatch_into_async_borrowed_resumable(
            &[0, 0, 0, 1, b'{'],
            &mut [],
        )) {
            ResumableDirectWrite::Overflow(pending) => pending,
            ResumableDirectWrite::Complete(_) => panic!("an empty buffer always overflows"),
        }
    }

    #[test]
    fn exact_flag_only_for_an_unclamped_size() {
        let max = usize::try_from(jint::MAX).unwrap();
        assert_eq!(handle_flags(max, true, false), FLAG_EXACT);
        assert_eq!(
            handle_flags(max + 1, true, false),
            0,
            "clamped size is not exact"
        );
        assert_eq!(handle_flags(usize::MAX, true, true), FLAG_STREAM);
        assert_eq!(handle_flags(10, false, true), FLAG_STREAM);
    }

    /// One test: the stash is process-global, so the scenarios must not
    /// interleave with each other.
    #[test]
    fn stash_handle_protocol_and_eviction() {
        // Handle written, redeemable exactly once.
        let mut out = vec![0u8; 64];
        let p = pending();
        let required = p.required_bytes();
        let code = stash_overflow(&mut out, p);
        assert_eq!(code, -jint::try_from(required).unwrap());
        let token = u64::from_be_bytes(out[..8].try_into().unwrap());
        assert_ne!(token, 0);
        assert_eq!(out[8] & FLAG_EXACT, FLAG_EXACT);
        assert_eq!(out[8] & FLAG_STREAM, 0);
        let token = jlong::try_from(token).unwrap();
        assert!(take_stashed(token).is_some());
        assert!(take_stashed(token).is_none(), "redeemed once");
        assert!(take_stashed(-1).is_none());

        // No room for a handle: nothing is written or kept.
        let mut tiny = vec![0xAAu8; 4];
        assert!(stash_overflow(&mut tiny, pending()) < 0);
        assert_eq!(tiny, [0xAA; 4]);
        assert!(lock_stash().entries.is_empty());

        // Surplus entries evict the oldest.
        let first = stash_put(None, pending());
        for _ in 0..MAX_STASHED {
            let _ = stash_put(None, pending());
        }
        assert!(stash_take(first).is_none(), "the oldest entry was evicted");
        assert_eq!(lock_stash().entries.len(), MAX_STASHED);
        clear_stash();
        assert!(lock_stash().entries.is_empty());

        // Eviction follows the stash time, not the token: a re-stashed
        // (partially fetched) low token outlives newer entries.
        let resumed = stash_put(None, pending());
        let newer = stash_put(None, pending());
        let _ = stash_put(Some(resumed), stash_take(resumed).expect("live"));
        for _ in 0..MAX_STASHED - 1 {
            let _ = stash_put(None, pending());
        }
        assert!(
            stash_take(newer).is_none(),
            "least recently stashed evicted"
        );
        assert!(stash_take(resumed).is_some(), "re-stashed entry kept");
        clear_stash();

        // Expired entries are swept by any access, not only by a put.
        let stale = Instant::now()
            .checked_sub(STASH_TTL + Duration::from_secs(1))
            .expect("monotonic clock far enough from its origin");
        let expired = stash_put(None, pending());
        let live = stash_put(None, pending());
        lock_stash().entries.get_mut(&expired).expect("stashed").0 = stale;
        assert!(stash_take(live).is_some());
        assert!(
            lock_stash().entries.is_empty(),
            "expired entry swept by a take"
        );
        clear_stash();

        // Byte budget: the oldest entries go first once the held bytes would
        // exceed it, and every removal releases its bytes.
        let each = pending().held_bytes();
        assert!(each > 0, "a materialised wire holds its bytes");
        let budget = each * 2;
        let first = stash_put_within(None, pending(), budget);
        let second = stash_put_within(None, pending(), budget);
        assert_eq!(stashed_bytes(), each * 2);
        let third = stash_put_within(None, pending(), budget);
        assert!(
            stash_take(first).is_none(),
            "oldest evicted over the budget"
        );
        assert_eq!(stashed_bytes(), each * 2);
        assert!(stash_take(second).is_some());
        assert!(stash_take(third).is_some());
        assert_eq!(stashed_bytes(), 0, "removals release their bytes");

        // A response over the whole budget is kept alone (refusing it would
        // make its handler run again); the entry before it is evicted.
        let over_first = stash_put_within(None, pending(), 0);
        let over_second = stash_put_within(None, pending(), 0);
        assert!(stash_take(over_first).is_none());
        assert!(stash_take(over_second).is_some(), "kept alone");
        assert_eq!(stashed_bytes(), 0);
        clear_stash();
    }

    fn wire_status(wire: &[u8]) -> String {
        let len = usize::try_from(u32::from_be_bytes(wire[..4].try_into().unwrap())).unwrap();
        String::from_utf8_lossy(&wire[4..4 + len]).into_owned()
    }

    /// Round 13.  Runs in a CHILD process: it builds the process-global
    /// shared runtime (which fixes the runtime config other tests set) and
    /// uses the global stash.
    #[test]
    fn resumable_dispatch_never_reruns_a_handler() {
        const CHILD_ENV: &str = "VESPERA_JNI_STASH_RESUMABLE_CHILD";
        const APP: &str = "stash_resumable_test";
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().expect("current unit-test executable"),
            )
            .args([
                "--exact",
                "jni_impl::direct::stash::tests::resumable_dispatch_never_reruns_a_handler",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("run isolated resumable child test");
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "isolated resumable test failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        vespera_inprocess::register_app_named(APP, || {
            vespera_inprocess::Router::new().route(
                "/panic",
                axum::routing::get(|| async {
                    CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    panic!("handler panic under test");
                }),
            )
        });
        let header = format!(r#"{{"v":1,"method":"GET","path":"/panic","app":"{APP}"}}"#);
        let mut request = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
        request.extend_from_slice(header.as_bytes());

        // An `out` without room for a handle asks for the minimum before
        // the handler runs.
        let mut tiny = [0xAAu8; STASH_HANDLE_LEN - 1];
        let code = dispatch_resumable(&request, &mut tiny).expect("runtime");
        assert_eq!(
            code,
            direct_overflow_code(vespera_inprocess::MIN_DIRECT_OUT_BYTES)
        );
        assert_eq!(CALLS.load(std::sync::atomic::Ordering::SeqCst), 0);

        // A panicking handler's `500` wire overflowing a small `out` is
        // stashed like any response: a handle, redeemable without a re-run.
        let mut small = [0u8; 64];
        let code = dispatch_resumable(&request, &mut small).expect("runtime");
        assert_eq!(CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
        let required = usize::try_from(-code).expect("an overflow");
        assert!(required > small.len(), "{code}");
        let token = u64::from_be_bytes(small[..8].try_into().unwrap());
        assert_ne!(token, 0, "the panic wire left a stash handle");
        let mut big = vec![0u8; required];
        let written = fetch_into(jlong::try_from(token).unwrap(), &mut big);
        assert_eq!(usize::try_from(written).unwrap(), required);
        let header = wire_status(&big);
        assert!(header.contains(r#""status":500"#), "{header}");
        assert!(header.contains(r#""panicked":"true""#), "{header}");
        assert_eq!(
            CALLS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "never re-run"
        );
        clear_stash();

        // An error wire that overflows `out` without a handle: `-(len)`
        // and a zeroed handle area, never `Integer.MIN_VALUE`.
        let wire = runtime_unavailable_wire();
        let mut out = [0xAAu8; 16];
        assert_eq!(
            write_error_wire(&mut out, &wire),
            direct_overflow_code(wire.len())
        );
        assert_eq!(out[..STASH_HANDLE_LEN], [0; STASH_HANDLE_LEN]);
        assert_eq!(out[STASH_HANDLE_LEN..], [0xAA; 16 - STASH_HANDLE_LEN]);
        let mut fits = vec![0u8; wire.len()];
        assert_eq!(
            write_error_wire(&mut fits, &wire),
            direct_complete_code(wire.len())
        );
        assert_eq!(fits, wire);
    }
}
