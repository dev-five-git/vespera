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
//! or released with `discard0`.  An unredeemed entry is kept until it is
//! redeemed or expires after [`STASH_TTL`] (swept on every stash access —
//! put, fetch, stream, discard).  A live entry is never evicted early: the
//! handler already ran for it, and evicting it would turn a correct response
//! into a failed redemption.  Only a fresh stash is refused, and only past
//! the hard ceilings [`MAX_STASHED`] / [`MAX_STASHED_BYTES`] (after the
//! expired entries are swept); a refused overflow answers a retryable `503`
//! wire.  One response larger than the byte ceiling is kept alone when the
//! stash is empty.  The byte count covers what an entry holds in memory (its
//! header and the body bytes already read), not the unread rest of a body.
//!
//! # Shutdown
//!
//! A shutdown closes the gate first and drains the in-flight dispatches
//! afterwards, so a DIRECT overflow of a dispatch still running during that
//! drain is stashed normally (it still holds its lease).  Only the
//! shutdown's cleanup ([`clear_stash`], run after the drain) closes the
//! stash: it drops every entry and refuses later ones, so an overflow after
//! it answers the retryable `503` wire instead of a handle nothing would
//! release.

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
/// Hard ceiling on the number of stashed responses.  Live entries are never
/// evicted early, so this only refuses a fresh stash (a retryable `503`);
/// it is a memory ceiling, not a working budget.
pub const MAX_STASHED: usize = 4096;
/// Hard ceiling on the bytes the stash holds across its entries (see
/// [`PendingDirectResponse::held_bytes`]).  A fresh stash that would exceed
/// it is refused, unless the stash is empty (then one response is kept alone:
/// refusing it would force the handler to run again).
pub const MAX_STASHED_BYTES: usize = 1024 * 1024 * 1024;

/// The ceilings a fresh stash must fit under ([`MAX_STASHED`] /
/// [`MAX_STASHED_BYTES`] in production; tests use small ones).
#[derive(Clone, Copy)]
struct Limits {
    entries: usize,
    bytes: usize,
}

const HARD_LIMITS: Limits = Limits {
    entries: MAX_STASHED,
    bytes: MAX_STASHED_BYTES,
};

/// Why a stash was not stored.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// [`clear_stash`] closed the stash (shutdown): the retryable `503`
    /// wire of [`runtime_unavailable_wire`].
    Closed,
    /// The hard ceilings are reached: a retryable `503` (overload).
    Full,
}

/// One stashed response: when it was stashed, the response, and the bytes
/// it was counted at (what [`Stash::held`] added for it).
type Entry = (Instant, PendingDirectResponse, usize);

struct Stash {
    next: u64,
    /// Token → entry.  A partially fetched response is re-stashed under its
    /// old token with a fresh timestamp, so key order is NOT timestamp
    /// order: expiry compares timestamps.
    entries: BTreeMap<u64, Entry>,
    /// Sum of the byte counts of `entries`.
    held: usize,
    /// Entries a fetch has taken out and not yet put back or released (see
    /// [`Stash::check_out`]); they still count toward the ceilings.
    checked_out: usize,
    /// Sum of the byte counts of the checked-out entries.
    checked_out_bytes: usize,
    /// Set by [`clear_stash`] (the shutdown's cleanup): no entry is taken
    /// after it, so nothing is stored that no cleanup would ever release.
    closed: bool,
}

impl Stash {
    /// Insert `pending` under `token`, counting its held bytes.  The entry
    /// it displaces (a re-stash over a live token) is returned, not dropped:
    /// the caller drops it after the lock is released.
    fn insert(
        &mut self,
        token: u64,
        at: Instant,
        pending: PendingDirectResponse,
    ) -> Option<(Instant, PendingDirectResponse)> {
        let bytes = pending.held_bytes();
        self.held += bytes;
        let (displaced_at, displaced, replaced) =
            self.entries.insert(token, (at, pending, bytes))?;
        self.held -= replaced;
        Some((displaced_at, displaced))
    }

    /// Store `pending` under `token` (`None` = a fresh token).  Expired
    /// entries go to `evicted` first; then a fresh stash is refused (the
    /// response comes back with the [`Refusal`]) when it would exceed
    /// `limits`, counting the checked-out entries too.  A re-put over a token
    /// always stores and is not re-checked: the fetch re-stashing it may hold
    /// more bytes than it checked out (its prefix grows by up to one caller
    /// buffer plus one body frame), so the byte ceiling is soft by that much
    /// per concurrent re-put.  A closed stash refuses everything.
    fn put(
        &mut self,
        token: Option<u64>,
        now: Instant,
        pending: PendingDirectResponse,
        limits: Limits,
        evicted: &mut Vec<(Instant, PendingDirectResponse)>,
    ) -> Result<u64, (Refusal, PendingDirectResponse)> {
        if self.closed {
            return Err((Refusal::Closed, pending));
        }
        self.sweep(now, evicted);
        let incoming = pending.held_bytes();
        let token = if let Some(token) = token {
            // Take the live entry out first: the re-put replaces it.
            evicted.extend(self.remove(token));
            token
        } else {
            let in_use = self.entries.len() + self.checked_out;
            let held = self.held + self.checked_out_bytes;
            let fits = in_use < limits.entries
                && (held == 0 || held.saturating_add(incoming) <= limits.bytes);
            if !fits {
                return Err((Refusal::Full, pending));
            }
            let token = self.next;
            self.next = self.next.wrapping_add(1).max(1);
            token
        };
        evicted.extend(self.insert(token, now, pending));
        Ok(token)
    }

    /// Remove `token`'s entry, releasing its counted bytes.
    fn remove(&mut self, token: u64) -> Option<(Instant, PendingDirectResponse)> {
        let (at, pending, bytes) = self.entries.remove(&token)?;
        self.held -= bytes;
        Some((at, pending))
    }

    /// Take `token`'s entry out for a fetch.  Its bytes move from `held` to
    /// `checked_out_bytes` and the entry counts as checked out, so a fresh
    /// stash cannot fill the ceilings while the fetch runs and then overshoot
    /// them with the re-put.  The fetch gives the count back through
    /// [`Stash::release_checkout`] (its re-put and its drop).
    fn check_out(&mut self, token: u64) -> Option<(PendingDirectResponse, usize)> {
        let (_, pending, bytes) = self.entries.remove(&token)?;
        self.held -= bytes;
        self.checked_out += 1;
        self.checked_out_bytes += bytes;
        Some((pending, bytes))
    }

    /// Release a [`Stash::check_out`] count of `bytes`.
    fn release_checkout(&mut self, bytes: usize) {
        self.checked_out -= 1;
        self.checked_out_bytes -= bytes;
    }

    /// Move every entry older than [`STASH_TTL`] into `evicted`.  The only
    /// eviction: a live entry is never dropped before its TTL.
    fn sweep(&mut self, now: Instant, evicted: &mut Vec<(Instant, PendingDirectResponse)>) {
        let expired: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, (at, _, _))| now.duration_since(*at) > STASH_TTL)
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            evicted.extend(self.remove(token));
        }
    }
}

static STASH: Mutex<Stash> = Mutex::new(Stash {
    next: 1,
    entries: BTreeMap::new(),
    held: 0,
    checked_out: 0,
    checked_out_bytes: 0,
    closed: false,
});

/// An entry a fetch took out of the stash (see [`Stash::check_out`]).  Its
/// count is released when it is dropped, so a fetch that ends early (a panic
/// unwinding out of the body read) cannot leave the ceilings reduced.
struct CheckedOut {
    token: u64,
    bytes: usize,
}

impl Drop for CheckedOut {
    fn drop(&mut self) {
        lock_stash().release_checkout(self.bytes);
    }
}

fn lock_stash() -> std::sync::MutexGuard<'static, Stash> {
    STASH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Store `pending` under `token` (`None` = a fresh token), sweeping expired
/// entries first.  `Err` when the stash is closed (after [`clear_stash`]) or
/// full (see [`Stash::put`]): the response is dropped and nothing is stored.
/// Swept and refused responses are dropped AFTER the lock is released
/// (dropping a body may run arbitrary code).
fn stash_put(token: Option<u64>, pending: PendingDirectResponse) -> Result<u64, Refusal> {
    stash_put_within(token, pending, HARD_LIMITS)
}

/// [`stash_put`] with explicit ceilings (tests use small ones).
fn stash_put_within(
    token: Option<u64>,
    pending: PendingDirectResponse,
    limits: Limits,
) -> Result<u64, Refusal> {
    let mut evicted = Vec::new();
    let outcome = lock_stash().put(token, Instant::now(), pending, limits, &mut evicted);
    drop(evicted);
    outcome.map_err(|(refusal, _refused)| refusal)
}

/// Remove `token`'s entry (if still live) and sweep expired ones, so an
/// abandoned response (and the body / connection it holds) never outlives
/// [`STASH_TTL`] past the next stash access.  Dropped after the lock.
fn stash_take(token: u64) -> Option<PendingDirectResponse> {
    let mut evicted = Vec::new();
    let removed = {
        let mut stash = lock_stash();
        stash.sweep(Instant::now(), &mut evicted);
        stash.remove(token)
    };
    drop(evicted);
    removed.map(|(_, pending)| pending)
}

/// Take a stashed response for streaming (`stream0`).
pub fn take_stashed(token: jlong) -> Option<PendingDirectResponse> {
    u64::try_from(token).ok().and_then(stash_take)
}

/// Take a stashed response for a fetch (`fetch0`), sweeping expired entries
/// first.  The entry keeps counting toward the ceilings until the returned
/// [`CheckedOut`] is dropped (see [`Stash::check_out`]).
fn check_out_stashed(token: jlong) -> Option<(CheckedOut, PendingDirectResponse)> {
    let token = u64::try_from(token).ok()?;
    let mut evicted = Vec::new();
    let checked = {
        let mut stash = lock_stash();
        stash.sweep(Instant::now(), &mut evicted);
        stash.check_out(token)
    };
    drop(evicted);
    checked.map(|(pending, bytes)| (CheckedOut { token, bytes }, pending))
}

/// Drop every stashed response and close the stash (the runtime shutdown's
/// cleanup, run after its drain): later [`stash_put`]s are refused.
pub fn clear_stash() {
    let drained = {
        let mut stash = lock_stash();
        stash.closed = true;
        stash.held = 0;
        std::mem::take(&mut stash.entries)
    };
    drop(drained);
}

/// Empty the stash and reopen it (tests only: several unit tests share the
/// process-global stash, and the real [`clear_stash`] closes it for good).
#[cfg(test)]
pub fn reset_stash() {
    let drained = {
        let mut stash = lock_stash();
        stash.held = 0;
        stash.closed = false;
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

/// `503` wire for an overflow the stash refused because it is full (every
/// live entry is still awaiting redemption, so none is evicted).  Retryable.
fn stash_full_wire() -> Vec<u8> {
    vespera_inprocess::error_wire(503, "DIRECT overflow stash is full; retry the request")
}

/// Stash an overflowing response and write its handle into `out`.
///
/// Called while the dispatch still holds its runtime lease (inside the
/// `block_on` future), so a shutdown drains it before its `clear_stash`
/// runs: the stash refuses entries only once that cleanup has closed it (see
/// the module docs).  An overflow after that, from a dispatch whose drain
/// timed out, is answered with the retryable `503` every other request gets,
/// not a handle to a response nothing would ever release.  An overflow the
/// full stash refuses (see the module docs) answers a `503` too.
pub fn stash_overflow(out: &mut [u8], pending: PendingDirectResponse) -> jint {
    let code = pending_code(&pending);
    let flags = pending_flags(&pending);
    if out.len() < STASH_HANDLE_LEN {
        // No room for a handle: the response cannot be redeemed, drop it.
        return code;
    }
    let token = match stash_put(None, pending) {
        Ok(token) => token,
        Err(Refusal::Closed) => return write_error_wire(out, &runtime_unavailable_wire()),
        Err(Refusal::Full) => return write_error_wire(out, &stash_full_wire()),
    };
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
    let Some((checked, pending)) = check_out_stashed(token) else {
        // `shutdown_runtime` clears the stash once it has drained the
        // leases — possibly between `dispatchResumable0` and this fetch:
        // answer the retryable `503` every other request gets, not "expired".
        if vespera_inprocess::runtime::runtime_shut_down() {
            return write_error_wire(out, &runtime_unavailable_wire());
        }
        return DIRECT_UNREPRESENTABLE;
    };
    // Re-stashed inside the future, under the fetch's runtime lease (see
    // `stash_overflow`); refused once the stash is closed.  The entry stayed
    // counted as checked out meanwhile, so its re-put never overshoots the
    // ceilings that concurrent fresh stashes respected.
    let fetched = block_on_shared_runtime(async {
        match pending.write_into(&mut *out).await {
            ResumableDirectWrite::Complete(n) => Some(direct_complete_code(n)),
            ResumableDirectWrite::Overflow(pending) => {
                let code = pending_code(&pending);
                stash_put(Some(checked.token), pending).ok().map(|_| code)
            }
        }
    });
    drop(checked);
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

    /// [`stash_put`] on the process-global stash (the tests reopen it first).
    fn put(token: Option<u64>, pending: PendingDirectResponse) -> u64 {
        stash_put(token, pending).expect("the stash is open and has room")
    }

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

    /// Ceilings for a test stash: `entries` count, `bytes` budget.
    const fn limits(entries: usize, bytes: usize) -> Limits {
        Limits { entries, bytes }
    }

    /// A stash that shares no state with the process-global one.
    fn local() -> Stash {
        Stash {
            next: 1,
            entries: BTreeMap::new(),
            held: 0,
            checked_out: 0,
            checked_out_bytes: 0,
            closed: false,
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
        reset_stash();

        // Live entries are never evicted early.  At the hard ceiling a fresh
        // overflow is refused with a retryable 503 wire, and every entry still
        // awaiting redemption stays redeemable (R41-J2-MED1).
        let live: Vec<u64> = (0..MAX_STASHED).map(|_| put(None, pending())).collect();
        assert_eq!(stashed_count(), MAX_STASHED);
        let mut out = vec![0u8; 1024];
        let code = stash_overflow(&mut out, pending());
        let len = usize::try_from(code).expect("a complete 503 wire");
        assert!(wire_status(&out[..len]).contains(r#""status":503"#));
        assert_eq!(stashed_count(), MAX_STASHED, "nothing was evicted");
        for token in live {
            assert!(
                stash_take(token).is_some(),
                "a live entry was evicted under pressure"
            );
        }
        assert_eq!(stashed_count(), 0);
        reset_stash();

        // Expired entries are swept by any access, not only by a put.
        let stale = Instant::now()
            .checked_sub(STASH_TTL + Duration::from_secs(1))
            .expect("monotonic clock far enough from its origin");
        let expired = put(None, pending());
        let live = put(None, pending());
        lock_stash().entries.get_mut(&expired).expect("stashed").0 = stale;
        assert!(stash_take(live).is_some());
        assert!(
            lock_stash().entries.is_empty(),
            "expired entry swept by a take"
        );
        reset_stash();

        // A re-stash over a live token displaces that entry: it leaves the
        // stash and its bytes are released (it is dropped after the lock).
        let each = pending().held_bytes();
        assert!(each > 0, "a materialised wire holds its bytes");
        let live = put(None, pending());
        let _ = put(Some(live), pending());
        assert_eq!(stashed_count(), 1, "the displaced entry left the stash");
        assert_eq!(stashed_bytes(), each, "its bytes were released");
        assert!(stash_take(live).is_some());
        assert_eq!(stashed_bytes(), 0, "removals release their bytes");
        reset_stash();

        // Closed by the shutdown's cleanup: nothing is stored (a new entry or a
        // re-stash), and an overflow answers the retryable 503 wire instead.
        clear_stash();
        assert_eq!(
            stash_put(None, pending()).err(),
            Some(Refusal::Closed),
            "closed refuses a new entry"
        );
        assert_eq!(stash_put(Some(7), pending()).err(), Some(Refusal::Closed));
        let mut out = vec![0u8; 1024];
        let code = stash_overflow(&mut out, pending());
        let len = usize::try_from(code).expect("a complete unavailable wire");
        assert_eq!(
            &out[..len],
            runtime_unavailable_wire().as_slice(),
            "the refused overflow answers the unavailable wire"
        );
        assert_eq!(stashed_count(), 0, "nothing stored after the close");
        reset_stash();
    }

    /// A re-put over a live token always stores (it replaces its own entry,
    /// so the count does not grow), even at the ceiling; nothing else leaves.
    /// Uses a local `Stash`, so it does not race the process-global one.
    #[test]
    fn re_put_at_the_ceiling_keeps_every_other_entry() {
        let mut stash = local();
        let now = Instant::now();
        let caps = limits(3, usize::MAX);
        let mut evicted = Vec::new();
        let tokens: Vec<u64> = (0..3)
            .map(|_| {
                stash
                    .put(None, now, pending(), caps, &mut evicted)
                    .expect("room")
            })
            .collect();
        let live = *tokens.last().expect("the ceiling is not empty");
        let token = stash
            .put(Some(live), now, pending(), caps, &mut evicted)
            .expect("a re-put is never refused");
        assert_eq!(token, live, "the re-put keeps its token");
        assert_eq!(evicted.len(), 1, "only the displaced live entry leaves");
        assert_eq!(stash.entries.len(), 3, "no entry was lost");
        for token in &tokens {
            assert!(
                stash.entries.contains_key(token),
                "an unrelated entry was evicted by the re-put"
            );
        }
    }

    /// A fetch that has taken its entry out keeps that entry counted: a fresh
    /// stash that the ceiling admits only without the checkout is refused
    /// while the fetch runs, and the fetch's re-put under its token stores
    /// anyway (its space was never given away).  Releasing the checkout then
    /// leaves exactly the re-stashed entries counted.  Local `Stash`, so it
    /// does not race the process-global one.
    #[test]
    fn checked_out_entry_counts_toward_the_ceilings_until_released() {
        let mut stash = local();
        let now = Instant::now();
        let caps = limits(2, usize::MAX);
        let mut evicted = Vec::new();
        let first = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        let (taken, bytes) = stash.check_out(first).expect("stashed");
        assert_eq!(stash.entries.len(), 1);
        assert_eq!(stash.checked_out, 1);

        let (refusal, refused) = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect_err("the checked-out entry counts toward the ceiling");
        assert_eq!(refusal, Refusal::Full);
        drop((refused, taken));

        let restashed = stash
            .put(Some(first), now, pending(), caps, &mut evicted)
            .expect("a re-put is never refused");
        assert_eq!(restashed, first);
        stash.release_checkout(bytes);
        assert_eq!(stash.checked_out, 0);
        assert_eq!(stash.checked_out_bytes, 0);
        assert_eq!(stash.entries.len(), 2);
        assert!(evicted.is_empty(), "nothing live was evicted");
    }

    /// The count ceiling refuses a fresh stash (`Full`) and hands the response
    /// back unstored; a live entry is never displaced to make room.
    #[test]
    fn full_stash_refuses_a_fresh_entry_and_keeps_the_live_ones() {
        let mut stash = local();
        let now = Instant::now();
        let caps = limits(2, usize::MAX);
        let mut evicted = Vec::new();
        let first = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        let second = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        let (refusal, refused) = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect_err("the ceiling is reached");
        assert_eq!(refusal, Refusal::Full);
        drop(refused);
        assert!(evicted.is_empty(), "nothing live was evicted");
        assert!(stash.entries.contains_key(&first));
        assert!(stash.entries.contains_key(&second));
    }

    /// The byte ceiling refuses a fresh stash that would exceed it, while a
    /// single response is kept alone when the stash is empty.
    #[test]
    fn byte_ceiling_refuses_a_fresh_entry_and_keeps_one_alone() {
        let mut stash = local();
        let now = Instant::now();
        let mut evicted = Vec::new();
        let each = pending().held_bytes();
        assert!(each > 0, "a materialised wire holds its bytes");
        let caps = limits(usize::MAX, each * 2);
        stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        assert_eq!(stash.held, each * 2);
        let (refusal, _refused) = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect_err("over the byte ceiling");
        assert_eq!(refusal, Refusal::Full);
        assert_eq!(stash.entries.len(), 2, "live entries kept");
        assert_eq!(stash.held, each * 2, "a refused entry counts nothing");

        let mut alone = local();
        let zero = limits(usize::MAX, 0);
        alone
            .put(None, now, pending(), zero, &mut evicted)
            .expect("kept alone in an empty stash");
        assert!(
            alone.put(None, now, pending(), zero, &mut evicted).is_err(),
            "not alone any more"
        );
        assert!(evicted.is_empty());
    }

    /// Expired entries are swept by a put, so they make room under the count
    /// ceiling; they are evicted, live ones never are.
    #[test]
    fn expired_entries_make_room_under_the_ceiling() {
        let mut stash = local();
        let now = Instant::now();
        let caps = limits(1, usize::MAX);
        let mut evicted = Vec::new();
        let stale = now
            .checked_sub(STASH_TTL + Duration::from_secs(1))
            .expect("monotonic clock far enough from its origin");
        let old = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("room");
        stash.entries.get_mut(&old).expect("stashed").0 = stale;
        let fresh = stash
            .put(None, now, pending(), caps, &mut evicted)
            .expect("the expired entry was swept, so there is room");
        assert_eq!(evicted.len(), 1, "the expired entry left");
        assert!(!stash.entries.contains_key(&old));
        assert!(stash.entries.contains_key(&fresh));
    }

    /// `Stash::insert` hands the displaced entry back (the caller drops it
    /// outside the lock) and keeps the held-byte count exact.  Uses a local
    /// `Stash`, so it does not race the process-global one.
    #[test]
    fn insert_returns_the_displaced_entry() {
        let mut stash = local();
        let now = Instant::now();
        let each = pending().held_bytes();
        assert!(stash.insert(5, now, pending()).is_none(), "fresh token");
        let displaced = stash.insert(5, now, pending());
        assert!(displaced.is_some(), "the entry at token 5 is returned");
        assert_eq!(stash.held, each, "the displaced bytes are released");
        assert_eq!(stash.entries.len(), 1);
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
        reset_stash();

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
