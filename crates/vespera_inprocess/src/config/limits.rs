//! Resource-limit knobs: buffered-response cap, streaming-request cap and
//! the per-dispatch deadline.  Split out of `config.rs` to keep that file
//! within the 1000-line source cap; same resolution policy as every other
//! knob there (setter > env var > default, then process-fixed via
//! `OnceLock`, invalid env values warn once and fall back to the default).

use std::sync::OnceLock;
use std::time::Duration;

use super::read_env_clamped;

/// Default cap on a **buffered** response body (256 MiB).
///
/// Buffered dispatch (`dispatch_from_bytes*`, `dispatchBytes`,
/// `dispatchAsync`) materialises the whole response in one `Vec`; without a
/// cap a handler streaming an unbounded body (or a bug producing one) grows
/// that `Vec` until the host process aborts on allocation failure.  256 MiB
/// is far above any sane buffered response — large payloads belong on the
/// streaming paths, which never materialise the body — while still bounding
/// the worst case.  `0` disables the cap.
pub const DEFAULT_MAX_BUFFERED_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Upper bound accepted for the dispatch timeout (30 days, in ms).  Anything
/// larger is clamped; it only exists so `Instant + timeout` arithmetic can
/// never overflow.
const MAX_DISPATCH_TIMEOUT_MS: usize = 30 * 24 * 60 * 60 * 1000;

static MAX_BUFFERED_RESPONSE_BYTES: OnceLock<usize> = OnceLock::new();
static MAX_STREAMING_REQUEST_BYTES: OnceLock<usize> = OnceLock::new();
static DISPATCH_TIMEOUT_MS: OnceLock<usize> = OnceLock::new();
static MAX_STREAMING_REQUEST_PRODUCERS: OnceLock<usize> = OnceLock::new();

/// Default cap on concurrently running bidirectional-streaming request
/// producers (256).
///
/// Each in-flight streamed upload parks one blocking-pool thread in the
/// host's blocking `read` (the JNI `InputStream.read`).  Without a cap a
/// burst of slow uploads can occupy the whole blocking pool (Tokio's default
/// 512) and starve the application's own `spawn_blocking` work (file I/O,
/// multipart temp files, blocking DB drivers).  256 leaves at least half of
/// the default pool to the application; a producer over the cap waits
/// (asynchronously, holding no thread) for a slot, which simply back-pressures
/// that upload.
pub const DEFAULT_MAX_STREAMING_REQUEST_PRODUCERS: usize = 256;
const MAX_STREAMING_REQUEST_PRODUCERS_LIMIT: usize = 1 << 20;

/// Maximum response **body** size the buffered dispatch paths will
/// materialise, in bytes.  `0` means unlimited.
///
/// Resolution: [`set_max_buffered_response_bytes`] >
/// `VESPERA_MAX_BUFFERED_RESPONSE_BYTES` env var >
/// [`DEFAULT_MAX_BUFFERED_RESPONSE_BYTES`] (256 MiB).
///
/// A response whose body exceeds the cap is replaced by a `500` wire
/// response naming the limit (nothing has been handed to the caller yet on
/// the buffered paths, so the substitution is clean).  The streaming paths
/// never materialise the body and are not affected.
///
/// The direct-write path ([`crate::dispatch_into_async`] and friends) is
/// bounded by the same cap: when a response with no exact size hint
/// overflows the caller's buffer, the remaining body is drained only to
/// count its exact size, and that counting drain stops with the same `500`
/// once the body exceeds this cap (with `0` = unlimited it is unbounded).
/// The resumable variant ([`crate::dispatch_into_async_borrowed_resumable`])
/// never drains past the caller's buffer at all — it hands the unread
/// remainder back as a [`crate::PendingDirectResponse`].
#[must_use]
#[inline]
pub fn max_buffered_response_bytes() -> usize {
    *MAX_BUFFERED_RESPONSE_BYTES.get_or_init(|| {
        read_env_clamped(
            "VESPERA_MAX_BUFFERED_RESPONSE_BYTES",
            DEFAULT_MAX_BUFFERED_RESPONSE_BYTES,
            0,
            usize::MAX,
        )
    })
}

/// Override the buffered-response cap **before the first dispatch**.
/// `0` means unlimited.  Returns `false` when the value was already fixed.
pub fn set_max_buffered_response_bytes(bytes: usize) -> bool {
    MAX_BUFFERED_RESPONSE_BYTES.set(bytes).is_ok()
}

/// Maximum total request-body bytes accepted on the **bidirectional
/// streaming** paths (the request body is pulled chunk-by-chunk, so it is
/// `O(chunk)` RAM regardless of size).  `0` (the default) means unlimited.
///
/// Resolution: [`set_max_streaming_request_bytes`] >
/// `VESPERA_MAX_STREAMING_REQUEST_BYTES` env var > `0`.
///
/// When exceeded, the request body errors out (the handler sees a failed
/// body, never a silently truncated one) and — if the response header has
/// not been committed yet — the response is replaced by a `413`.
#[must_use]
#[inline]
pub fn max_streaming_request_bytes() -> usize {
    *MAX_STREAMING_REQUEST_BYTES
        .get_or_init(|| read_env_clamped("VESPERA_MAX_STREAMING_REQUEST_BYTES", 0, 0, usize::MAX))
}

/// Override the streaming request-body cap **before the first dispatch**.
/// `0` means unlimited.  Returns `false` when the value was already fixed.
pub fn set_max_streaming_request_bytes(bytes: usize) -> bool {
    MAX_STREAMING_REQUEST_BYTES.set(bytes).is_ok()
}

/// Maximum number of bidirectional-streaming request producers (blocking
/// upload readers) running at once.  `0` means unlimited.
///
/// Resolution: [`set_max_streaming_request_producers`] >
/// `VESPERA_MAX_STREAMING_REQUEST_PRODUCERS` env var >
/// [`DEFAULT_MAX_STREAMING_REQUEST_PRODUCERS`] (256).  A producer over the
/// cap is not rejected: it waits for a free slot before it is started.
#[must_use]
#[inline]
pub fn max_streaming_request_producers() -> usize {
    *MAX_STREAMING_REQUEST_PRODUCERS.get_or_init(|| {
        read_env_clamped(
            "VESPERA_MAX_STREAMING_REQUEST_PRODUCERS",
            DEFAULT_MAX_STREAMING_REQUEST_PRODUCERS,
            0,
            MAX_STREAMING_REQUEST_PRODUCERS_LIMIT,
        )
    })
}

/// Override the request-producer cap **before the first bidirectional
/// streaming dispatch**.  `0` means unlimited.  Returns `false` when the
/// value was already fixed.
pub fn set_max_streaming_request_producers(producers: usize) -> bool {
    MAX_STREAMING_REQUEST_PRODUCERS
        .set(producers.min(MAX_STREAMING_REQUEST_PRODUCERS_LIMIT))
        .is_ok()
}

fn dispatch_timeout_ms() -> usize {
    *DISPATCH_TIMEOUT_MS.get_or_init(|| {
        read_env_clamped("VESPERA_DISPATCH_TIMEOUT_MS", 0, 0, MAX_DISPATCH_TIMEOUT_MS)
    })
}

/// Per-dispatch deadline, or `None` when disabled (the default).
///
/// Resolution: [`set_dispatch_timeout`] > `VESPERA_DISPATCH_TIMEOUT_MS`
/// env var (milliseconds) > `0` (disabled).  Clamped to 30 days.
///
/// When set, every wire dispatch races the deadline:
///
/// * **buffered / direct-write** dispatch → the whole dispatch (handler +
///   body collection) is bounded; expiry → `504` wire response;
/// * **streaming** dispatch → only the time to the response header
///   (time-to-first-header) is bounded; expiry → `504` delivered as the
///   header.  Once the header is committed the body streams without a
///   total deadline, so a long download that keeps making progress is
///   never cut off.
///
/// The handler future is dropped at the deadline (Tokio cancellation).
/// **Requires the Tokio time driver** (`enable_time()` / `enable_all()`)
/// on the runtime driving the dispatch once enabled; the JNI runtime has it.
#[must_use]
#[inline]
pub fn dispatch_timeout() -> Option<Duration> {
    match dispatch_timeout_ms() {
        0 => None,
        ms => Some(Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX))),
    }
}

/// Override the per-dispatch deadline **before the first dispatch**.
/// [`Duration::ZERO`] disables it.  Sub-millisecond precision is
/// truncated (a non-zero value below 1 ms rounds up to 1 ms so it never
/// silently disables the deadline).  Returns `false` when already fixed.
pub fn set_dispatch_timeout(timeout: Duration) -> bool {
    let ms = if timeout.is_zero() {
        0
    } else {
        usize::try_from(timeout.as_millis())
            .unwrap_or(usize::MAX)
            .clamp(1, MAX_DISPATCH_TIMEOUT_MS)
    };
    DISPATCH_TIMEOUT_MS.set(ms).is_ok()
}

#[cfg(test)]
mod tests {
    use super::super::parse_config_value;
    use super::{
        DEFAULT_MAX_BUFFERED_RESPONSE_BYTES, MAX_DISPATCH_TIMEOUT_MS, dispatch_timeout,
        max_buffered_response_bytes, max_streaming_request_bytes, max_streaming_request_producers,
        set_dispatch_timeout, set_max_buffered_response_bytes, set_max_streaming_request_bytes,
        set_max_streaming_request_producers,
    };
    use rstest::rstest;
    use std::time::Duration;

    /// Process-global `OnceLock`s: run in a child process so the setter
    /// assertions cannot race other unit tests that read the defaults.
    #[test]
    fn limit_setters_are_first_wins_and_feed_getters() {
        const CHILD_ENV: &str = "VESPERA_LIMITS_SETTER_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().expect("current unit-test executable"),
            )
            .args([
                "--exact",
                "config::limits::tests::limit_setters_are_first_wins_and_feed_getters",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("run isolated limits child test");
            assert!(
                output.status.success(),
                "isolated limits test failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        assert!(set_max_buffered_response_bytes(1234));
        assert!(set_max_streaming_request_bytes(99));
        assert!(set_dispatch_timeout(Duration::from_micros(10)));
        assert!(set_max_streaming_request_producers(3));
        assert_eq!(max_streaming_request_producers(), 3);
        assert!(!set_max_streaming_request_producers(4));

        assert_eq!(max_buffered_response_bytes(), 1234);
        assert_eq!(max_streaming_request_bytes(), 99);
        assert_eq!(
            dispatch_timeout(),
            Some(Duration::from_millis(1)),
            "a non-zero sub-ms timeout rounds up instead of disabling"
        );

        assert!(!set_max_buffered_response_bytes(1));
        assert!(!set_max_streaming_request_bytes(1));
        assert!(!set_dispatch_timeout(Duration::ZERO));
    }

    #[rstest]
    #[case::absent(None, DEFAULT_MAX_BUFFERED_RESPONSE_BYTES)]
    #[case::invalid(Some("256MiB"), DEFAULT_MAX_BUFFERED_RESPONSE_BYTES)]
    #[case::explicit_unlimited(Some("0"), 0)]
    #[case::explicit(Some(" 1048576 "), 1_048_576)]
    fn buffered_response_cap_policy(#[case] raw: Option<&str>, #[case] expected: usize) {
        assert_eq!(
            parse_config_value(
                "VESPERA_MAX_BUFFERED_RESPONSE_BYTES",
                raw,
                DEFAULT_MAX_BUFFERED_RESPONSE_BYTES,
                0,
                usize::MAX
            ),
            expected
        );
    }

    #[rstest]
    #[case::absent(None, 0)]
    #[case::invalid(Some("5s"), 0)]
    #[case::explicit(Some("2500"), 2500)]
    #[case::clamped(Some("99999999999999"), MAX_DISPATCH_TIMEOUT_MS)]
    fn dispatch_timeout_policy(#[case] raw: Option<&str>, #[case] expected: usize) {
        assert_eq!(
            parse_config_value(
                "VESPERA_DISPATCH_TIMEOUT_MS",
                raw,
                0,
                0,
                MAX_DISPATCH_TIMEOUT_MS
            ),
            expected
        );
    }
}
