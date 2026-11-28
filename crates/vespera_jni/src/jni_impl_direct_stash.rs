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
//! the stash handle into `out[0..9]` (an `out` below
//! `vespera_inprocess::MIN_DIRECT_OUT_BYTES` (1 KiB) is answered
//! `-(MIN_DIRECT_OUT_BYTES)` without dispatching, as in `dispatchDirect0`;
//! a handler panic's `500` wire is stashed like any response, so the handler
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
    run_direct_symbol, undersized_out_code,
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
/// The same 1 KiB minimum as `dispatchDirect0` ([`undersized_out_code`]):
/// an `out` below [`vespera_inprocess::MIN_DIRECT_OUT_BYTES`] asks for that
/// size without dispatching (the input checks of `run_direct_symbol` run
/// first).  Every synthesized wire (the `500` panic wire, `504`, the `503`
/// of a refused stash, ...) fits that minimum, so an error wire never
/// overflows `out` after a handler ran.  Otherwise a retry would re-run the
/// handler whose response was lost: that is why the minimum matters, and
/// why the error-wire path ([`write_error_wire`]) is only a guard here.
/// Any response produced after the handler ran (its own, or the panic wire
/// of a panicking handler) that overflows `out` is stashed, so the handler
/// never re-runs.
fn dispatch_resumable(input: &[u8], out: &mut [u8]) -> Option<jint> {
    if let Some(code) = undersized_out_code(out.len()) {
        // Zero the handle area like the error-wire path does
        // (`write_error_to_out`): a caller reads an overflow handle from
        // there, and must never take a stale token for this call's.
        let handle_len = out.len().min(STASH_HANDLE_LEN);
        out[..handle_len].fill(0);
        return Some(code);
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
#[path = "jni_impl_direct_stash_tests.rs"]
mod tests;
