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
//! `-(required)` (clamped to `i32::MAX`; exact only when flagged) and, when
//! `out` holds at least [`STASH_HANDLE_LEN`] bytes, writes the stash handle
//! into `out[0..9]`: an 8-byte big-endian token (never `0`) followed by a
//! flags byte ([`FLAG_EXACT`], [`FLAG_STREAM`]).  The token is redeemed with
//! `fetch0` (copy into a larger buffer), `stream0` (header-first streaming)
//! or released with `discard0`.  An unredeemed entry expires after
//! [`STASH_TTL`] (swept on every stash access — put, fetch, stream,
//! discard); at most [`MAX_STASHED`] entries are kept (least recently
//! stashed evicted).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jni::EnvUnowned;
use jni::objects::{JByteBuffer, JClass};
use jni::sys::{jint, jlong};
use vespera_inprocess::{PendingDirectResponse, ResumableDirectWrite};

use super::{
    DIRECT_UNREPRESENTABLE, block_on_shared_runtime, direct_complete_code, run_direct_symbol,
};
use crate::jni_impl::{guard_void_symbol, runtime_unavailable_wire};

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

struct Stash {
    next: u64,
    /// Token → (stashed at, response).  A partially fetched response is
    /// re-stashed under its old token with a fresh timestamp, so key order
    /// is NOT timestamp order: expiry and eviction compare timestamps.
    entries: BTreeMap<u64, (Instant, PendingDirectResponse)>,
}

impl Stash {
    /// Move every entry older than [`STASH_TTL`] into `evicted`, then — when
    /// `make_room` — the least recently stashed ones until one more fits.
    fn evict(
        &mut self,
        now: Instant,
        make_room: bool,
        evicted: &mut Vec<(Instant, PendingDirectResponse)>,
    ) {
        let expired: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, (at, _))| now.duration_since(*at) > STASH_TTL)
            .map(|(token, _)| *token)
            .collect();
        evicted.extend(
            expired
                .iter()
                .filter_map(|token| self.entries.remove(token)),
        );
        while make_room && self.entries.len() >= MAX_STASHED {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(token, _)| *token)
            else {
                break;
            };
            evicted.extend(self.entries.remove(&oldest));
        }
    }
}

static STASH: Mutex<Stash> = Mutex::new(Stash {
    next: 1,
    entries: BTreeMap::new(),
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
    let now = Instant::now();
    let mut evicted = Vec::new();
    let token = {
        let mut stash = lock_stash();
        stash.evict(now, true, &mut evicted);
        let token = token.unwrap_or_else(|| {
            let token = stash.next;
            stash.next = stash.next.wrapping_add(1).max(1);
            token
        });
        stash.entries.insert(token, (now, pending));
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
        stash.evict(Instant::now(), false, &mut evicted);
        stash.entries.remove(&token)
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
    let drained = std::mem::take(&mut lock_stash().entries);
    drop(drained);
}

/// `-(required)`, clamped so it is never the `MIN` sentinel.
fn pending_code(pending: &PendingDirectResponse) -> jint {
    -jint::try_from(pending.required_bytes()).unwrap_or(jint::MAX)
}

fn pending_flags(pending: &PendingDirectResponse) -> u8 {
    let mut flags = 0;
    if pending.is_exact() {
        flags |= FLAG_EXACT;
    }
    if pending.prefers_streaming() {
        flags |= FLAG_STREAM;
    }
    flags
}

/// Stash an overflowing response and write its handle into `out`.
fn stash_overflow(out: &mut [u8], pending: PendingDirectResponse) -> jint {
    let code = pending_code(&pending);
    let flags = pending_flags(&pending);
    let Some(handle) = out.get_mut(..STASH_HANDLE_LEN) else {
        // No room for a handle: the response cannot be redeemed, drop it.
        return code;
    };
    let token = stash_put(None, pending);
    handle[..8].copy_from_slice(&token.to_be_bytes());
    handle[8] = flags;
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
    run_direct_symbol(unowned_env, &in_buf, in_len, &out_buf, |input, out| {
        block_on_shared_runtime(vespera_inprocess::dispatch_into_async_borrowed_resumable(
            input, &mut *out,
        ))
        .map(|written| match written {
            ResumableDirectWrite::Complete(n) => direct_complete_code(n),
            ResumableDirectWrite::Overflow(pending) => stash_overflow(out, pending),
        })
    })
}

/// `com.devfive.vespera.bridge.DirectStash.fetch0(long, ByteBuffer) -> int`
///
/// Continue the stashed response `token` into the direct buffer `out`:
/// `>= 0` complete (the entry is consumed), `-(required)` still too large
/// (the entry is kept under the same token, with more of the body read),
/// `Integer.MIN_VALUE` when the token is unknown (expired / evicted /
/// already redeemed) or `out` is not a direct buffer.  A configured
/// dispatch timeout bounds the call (`504` wire).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_DirectStash_fetch0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    token: jlong,
    out_buf: JByteBuffer<'local>,
) -> jint {
    let mut code = DIRECT_UNREPRESENTABLE;
    let _ = guard_void_symbol(|| {
        let _ = unowned_env.with_env(|env| -> jni::errors::Result<()> {
            if out_buf.is_null() {
                return Ok(());
            }
            let out_addr = env.get_direct_buffer_address(&out_buf)?;
            let out_cap = env.get_direct_buffer_capacity(&out_buf)?;
            let Some(pending) = take_stashed(token) else {
                return Ok(());
            };
            // SAFETY: `out_buf` is a live direct buffer (pinned by its local
            // ref for the whole call) covering `out_cap` writable bytes —
            // the Java caller only passes its own pooled direct buffers.
            // The slice never escapes this synchronous `block_on`.
            let out = unsafe { std::slice::from_raw_parts_mut(out_addr, out_cap) };
            code = match block_on_shared_runtime(pending.write_into(&mut *out)) {
                Some(ResumableDirectWrite::Complete(n)) => direct_complete_code(n),
                Some(ResumableDirectWrite::Overflow(pending)) => {
                    let code = pending_code(&pending);
                    let key = u64::try_from(token).unwrap_or_default();
                    stash_put(Some(key), pending);
                    code
                }
                // No runtime (shut down): the stashed response is dropped;
                // answer with the runtime-unavailable wire when it fits.
                None => {
                    let wire = runtime_unavailable_wire();
                    out.get_mut(..wire.len())
                        .map_or(DIRECT_UNREPRESENTABLE, |dst| {
                            dst.copy_from_slice(&wire);
                            direct_complete_code(wire.len())
                        })
                }
            };
            Ok(())
        });
    });
    code
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
    }
}
