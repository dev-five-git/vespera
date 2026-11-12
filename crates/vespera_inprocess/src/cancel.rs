//! Host-driven cancellation of in-flight dispatch futures.
//!
//! An FFI host that spawns a dispatch and hands back a completion handle
//! (the JNI `dispatchAsync` `CompletableFuture`) needs a way to say "the
//! caller is gone, stop working": the HTTP client disconnected, an async
//! request timed out, or the caller cancelled its future.  [`cancellable`]
//! wraps a future and registers it under a non-zero `u64` token the host can
//! hold as a plain integer; [`cancel`] (from any thread) makes the wrapped
//! future resolve to `None` on its next poll — the inner future (and with it
//! the axum handler and everything it owns: DB connections, timers, spawned
//! child guards) is **dropped** right away instead of running to completion.
//!
//! Race-free by construction:
//! - the token is registered **before** the caller spawns the future, and the
//!   future deregisters it when it is dropped (completed, cancelled, or torn
//!   down with the runtime), so an entry can never outlive its future;
//! - cancelling a finished or unknown token is a harmless `false`;
//! - the wake-up is a `tokio::sync::oneshot` whose sender lives in the
//!   registry: [`cancel`] removes and drops it, which wakes the receiver even
//!   when the future is not currently parked on it.
//!
//! The registry is sharded so concurrent registrations from many host
//! threads rarely contend on the same lock.

use std::{
    collections::HashMap,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    task::Poll,
};

use tokio::sync::oneshot;

const SHARDS: usize = 16;

type Shard = Mutex<HashMap<u64, oneshot::Sender<Infallible>, NoHash>>;

static REGISTRY: [Shard; SHARDS] = [const { Mutex::new(HashMap::with_hasher(NoHash)) }; SHARDS];
/// Next token; starts at 1 so `0` can mean "not cancellable" to hosts.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Tokens are unique sequential integers, so they need no hashing; an
/// identity hasher also keeps the shard maps `const`-constructible.
#[derive(Clone, Copy, Default)]
struct NoHash;

impl std::hash::BuildHasher for NoHash {
    type Hasher = IdentityHasher;

    fn build_hasher(&self) -> IdentityHasher {
        IdentityHasher(0)
    }
}

#[derive(Default)]
struct IdentityHasher(u64);

impl std::hash::Hasher for IdentityHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Only `write_u64` is ever used (keys are `u64`); fold any other
        // input so the hasher stays total.
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

fn shard(token: u64) -> &'static Shard {
    // `SHARDS` is a power of two; the modulo keeps the index in range.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is reduced modulo SHARDS (16) first"
    )]
    &REGISTRY[(token % SHARDS as u64) as usize]
}

/// Removes the token from the registry when the wrapped future is dropped,
/// on every path: completed, cancelled, or never polled.
struct Deregister(u64);

impl Drop for Deregister {
    fn drop(&mut self) {
        shard(self.0)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Wrap `future` so the host can cancel it with [`cancel`].
///
/// Returns the non-zero token and a future that resolves to `Some(output)`
/// when `future` completes, or to `None` once the token was cancelled (the
/// inner future is dropped at that point, without being polled again).
/// Register-then-spawn: the token is live as soon as this returns, so the
/// host may cancel even before the returned future is first polled.
pub fn cancellable<F: Future>(future: F) -> (u64, impl Future<Output = Option<F::Output>>) {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = oneshot::channel::<Infallible>();
    shard(token)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(token, tx);
    let guard = Deregister(token);
    let wrapped = async move {
        let _guard = guard;
        let mut future = std::pin::pin!(future);
        // On `None` this async block returns at once, dropping the pinned
        // inner future (and the guard) without polling it again.
        std::future::poll_fn(|cx| {
            // Cancellation wins over a simultaneously ready inner future:
            // the host already gave up on the result.  The sender is only
            // ever dropped (never sends), so a ready receiver means
            // "cancelled".
            if Pin::new(&mut rx).poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            future.as_mut().poll(cx).map(Some)
        })
        .await
    };
    (token, wrapped)
}

/// Cancel the future registered under `token` (see [`cancellable`]).
///
/// Returns `true` when a live registration was found — its future resolves
/// to `None` on its next poll (it is woken now) and drops the inner work.
/// Returns `false` for `0`, an unknown token, or one whose future already
/// finished or was dropped: cancelling late is always harmless.
pub fn cancel(token: u64) -> bool {
    shard(token)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&token)
        .is_some()
}

/// Number of currently registered (live, not yet cancelled) cancellable
/// futures across all shards — a diagnostic for leak checks.
pub fn cancellable_in_flight() -> usize {
    REGISTRY
        .iter()
        .map(|s| s.lock().unwrap_or_else(PoisonError::into_inner).len())
        .sum()
}

#[cfg(test)]
mod tests {
    use std::{
        hash::{BuildHasher, Hasher},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use super::*;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn registered(token: u64) -> bool {
        shard(token)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&token)
    }

    #[tokio::test]
    async fn completes_with_output_and_deregisters() {
        let (token, fut) = cancellable(async { 7 });
        assert_ne!(token, 0);
        assert!(registered(token));
        assert_eq!(fut.await, Some(7));
        assert!(!registered(token), "finished futures deregister");
        assert!(!cancel(token), "cancelling a finished dispatch is a no-op");
    }

    #[tokio::test]
    async fn cancel_drops_the_pending_inner_future_promptly() {
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = DropFlag(Arc::clone(&dropped));
        let (token, fut) = cancellable(async move {
            let _flag = flag;
            std::future::pending::<()>().await;
        });
        let task = tokio::spawn(fut);
        tokio::task::yield_now().await;
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(cancel(token));
        assert_eq!(task.await.unwrap(), None);
        assert!(dropped.load(Ordering::SeqCst), "inner future dropped");
        assert!(!registered(token));
        assert!(!cancel(token), "second cancel is a no-op");
    }

    #[tokio::test]
    async fn cancel_before_first_poll_never_runs_the_inner_future() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);
        let (token, fut) = cancellable(async move { ran2.store(true, Ordering::SeqCst) });
        assert!(cancel(token));
        assert_eq!(fut.await, None);
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn dropping_an_unpolled_future_deregisters() {
        let (token, fut) = cancellable(async {});
        assert!(registered(token));
        // Other tests run concurrently, so only a lower bound is stable.
        assert!(cancellable_in_flight() >= 1);
        drop(fut);
        assert!(!registered(token));
    }

    /// `cancel` from another OS thread racing the future's own completion
    /// (the `AsyncDispatch.cancel0` vs runtime-worker race).  Either side may
    /// win — a late cancel can even report `true` while the output is
    /// `Some`, harmless because the host already gave up — but the entry is
    /// always gone afterwards and the registry / oneshot hand-off is
    /// race-free (run under Miri, which checks the latter).
    #[test]
    fn cancel_racing_completion_from_another_thread_leaves_no_entry() {
        for _ in 0..4 {
            let (token, fut) = cancellable(async { 7 });
            let racer = std::thread::spawn(move || cancel(token));
            let output = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime")
                .block_on(fut);
            let won = racer.join().unwrap();
            assert!(output == Some(7) || (output.is_none() && won));
            assert!(!registered(token));
        }
    }

    #[test]
    fn unknown_and_zero_tokens_are_not_cancellable() {
        assert!(!cancel(0));
        assert!(!cancel(u64::MAX));
    }

    #[test]
    fn identity_hasher_is_total() {
        let mut h = NoHash.build_hasher();
        h.write_u64(42);
        assert_eq!(h.finish(), 42);
        let mut h = IdentityHasher::default();
        h.write(&[1, 2]);
        assert_eq!(h.finish(), (1u64 << 8) ^ 2);
    }
}
