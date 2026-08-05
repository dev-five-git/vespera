//! Process-wide streaming configuration (chunk size, channel
//! capacity) — resolved once via `OnceLock`: setter > env > default.

use std::sync::OnceLock;

// ── Streaming Configuration ──────────────────────────────────────────

/// Default per-chunk buffer size for streaming dispatches (256 KiB).
///
/// Large enough to amortise per-chunk FFI overhead (JNI region copy +
/// `OutputStream.write` call per chunk), small enough to keep memory
/// bounded for multi-GB streams.  Raised from 64 KiB to 256 KiB
/// because measured streaming throughput improves ~25 % (11.6 → 14.5
/// GB/s) at the cost of an extra 192 KiB of per-stream buffer per
/// direction — both still well within "low-single-digit MiB resident
/// per stream" for multi-GB transfers.  Tune down via
/// `set_streaming_chunk_bytes`, the `VESPERA_STREAMING_CHUNK_BYTES`
/// env var, or `VesperaBridge.configureStreaming(...)` when memory is
/// tighter than throughput.
pub const DEFAULT_STREAMING_CHUNK_BYTES: usize = 256 * 1024;

/// Default capacity (slots) of the bounded mpsc channel that feeds
/// request-body chunks into axum during bidirectional streaming.
pub const DEFAULT_STREAMING_CHANNEL_CAPACITY: usize = 16;

const MIN_STREAMING_CHUNK_BYTES: usize = 4 * 1024;
const MAX_STREAMING_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MIN_STREAMING_CHANNEL_CAPACITY: usize = 1;
const MAX_STREAMING_CHANNEL_CAPACITY: usize = 1024;

static STREAMING_CHUNK_BYTES: OnceLock<usize> = OnceLock::new();
static STREAMING_CHANNEL_CAPACITY: OnceLock<usize> = OnceLock::new();

/// Parse an optional config string into a clamped `usize`, falling back to
/// `default` when the value is **absent**.
///
/// A value that is **present but unparseable** (e.g. a typo like `"256KiB"` or
/// `"abc"`) emits a one-time stderr warning — every caller resolves through a
/// process-`OnceLock`, so its initializer runs at most once — and then uses
/// `default`. This mirrors [`max_request_bytes`]'s warn-and-default policy so a
/// mistuned streaming knob is never silently ignored (the operator would
/// otherwise believe they tuned a value that is actually unchanged).
fn parse_config_value(
    var_name: &str,
    raw: Option<&str>,
    default: usize,
    min: usize,
    max: usize,
) -> usize {
    raw.map_or(default, |s| {
        s.trim().parse::<usize>().map_or_else(
            |_| {
                eprintln!(
                    "vespera: ignoring invalid {var_name}={s:?} \
                     (expected a non-negative integer); using the default {default}"
                );
                default
            },
            |v| v.clamp(min, max),
        )
    })
}

/// Look up `var_name` in the process environment and delegate to
/// [`parse_config_value`] — the shared **production** entry point that
/// keeps `var_name` written **once per call site**.
///
/// Prior shape passed the env-var name **twice** to the same call
/// (`parse_config_value(name, std::env::var(name).ok().as_deref(), ...)`),
/// so a rename that touched only one occurrence would silently swap the
/// stderr warn message off the variable actually being read — a real
/// observability drift.  Delegating through this helper collapses both
/// literal occurrences into one.
///
/// [`parse_config_value`] stays as the pure, `Option<&str>`-taking
/// predicate the existing unit tests exercise; this thin wrapper adds
/// only the `std::env::var` lookup and is a straight-line delegate.
fn read_env_clamped(var_name: &'static str, default: usize, min: usize, max: usize) -> usize {
    let raw = std::env::var(var_name).ok();
    parse_config_value(var_name, raw.as_deref(), default, min, max)
}

/// Effective per-chunk buffer size for streaming dispatches.
///
/// Resolution order (first hit wins, then cached for the process
/// lifetime via `OnceLock` — a single atomic load per call):
///
/// 1. [`set_streaming_chunk_bytes`] called before the first read
/// 2. `VESPERA_STREAMING_CHUNK_BYTES` environment variable
/// 3. [`DEFAULT_STREAMING_CHUNK_BYTES`] (256 KiB)
///
/// Values are clamped to `[4 KiB, 8 MiB]`.
#[must_use]
#[inline]
pub fn streaming_chunk_bytes() -> usize {
    *STREAMING_CHUNK_BYTES.get_or_init(|| {
        read_env_clamped(
            "VESPERA_STREAMING_CHUNK_BYTES",
            DEFAULT_STREAMING_CHUNK_BYTES,
            MIN_STREAMING_CHUNK_BYTES,
            MAX_STREAMING_CHUNK_BYTES,
        )
    })
}

/// Override the streaming chunk size **before the first dispatch**
/// (e.g. from a host-language configuration hook at init time).
///
/// Returns `false` when the value was already fixed — either by a
/// previous call or because a dispatch has already read it.  The
/// supplied value is clamped to `[4 KiB, 8 MiB]`.
pub fn set_streaming_chunk_bytes(bytes: usize) -> bool {
    STREAMING_CHUNK_BYTES
        .set(bytes.clamp(MIN_STREAMING_CHUNK_BYTES, MAX_STREAMING_CHUNK_BYTES))
        .is_ok()
}

/// Effective bound (slots) of the bidirectional request-body channel.
///
/// Same resolution order as [`streaming_chunk_bytes`]:
/// [`set_streaming_channel_capacity`] >
/// `VESPERA_STREAMING_CHANNEL_CAPACITY` env var >
/// [`DEFAULT_STREAMING_CHANNEL_CAPACITY`] (16).  Clamped to
/// `[1, 1024]`.
#[must_use]
#[inline]
pub fn streaming_channel_capacity() -> usize {
    *STREAMING_CHANNEL_CAPACITY.get_or_init(|| {
        read_env_clamped(
            "VESPERA_STREAMING_CHANNEL_CAPACITY",
            DEFAULT_STREAMING_CHANNEL_CAPACITY,
            MIN_STREAMING_CHANNEL_CAPACITY,
            MAX_STREAMING_CHANNEL_CAPACITY,
        )
    })
}

/// Override the bidirectional channel capacity **before the first
/// dispatch**.  Returns `false` when already fixed.  Clamped to
/// `[1, 1024]`.
pub fn set_streaming_channel_capacity(slots: usize) -> bool {
    STREAMING_CHANNEL_CAPACITY
        .set(slots.clamp(
            MIN_STREAMING_CHANNEL_CAPACITY,
            MAX_STREAMING_CHANNEL_CAPACITY,
        ))
        .is_ok()
}

/// Hard ceiling on the peak request-body bytes buffered in the
/// bidirectional-streaming mpsc channel at any instant. The channel holds up
/// to `channel_capacity` chunks, each up to `chunk_bytes`, so peak buffered
/// memory is `chunk_bytes * channel_capacity`. With BOTH knobs at their
/// maxima (8 MiB * 1024) that product is **8 GiB** — which defeats streaming's
/// `O(chunk)` RAM goal and can OOM a host under concurrent uploads.
/// [`effective_streaming_channel_capacity`] clamps the in-flight slot count so
/// this product is never exceeded.
const MAX_STREAMING_BUFFERED_BYTES: usize = 64 * 1024 * 1024;

/// Effective in-flight slot count for the bidirectional request-body channel:
/// [`streaming_channel_capacity`] clamped so that
/// `chunk_bytes * slots <= MAX_STREAMING_BUFFERED_BYTES`.
///
/// This adapts to the configured chunk size — a large chunk yields fewer
/// slots — so peak buffered request memory per stream stays bounded no matter
/// how the two knobs are set. The configured capacity is honoured unchanged
/// when it is already within budget (the common default 256 KiB * 16 = 4 MiB
/// is far under the 64 MiB ceiling). The floor is 1 slot so streaming always
/// makes progress even with an 8 MiB chunk.
#[must_use]
#[inline]
pub fn effective_streaming_channel_capacity() -> usize {
    cap_channel_slots(
        streaming_channel_capacity(),
        streaming_chunk_bytes(),
        MAX_STREAMING_BUFFERED_BYTES,
    )
}

/// Pure product-cap math behind [`effective_streaming_channel_capacity`],
/// split out so the clamp behaviour is unit-testable without the
/// process-global `OnceLock` config (which can only be set once per process).
fn cap_channel_slots(configured: usize, chunk_bytes: usize, max_buffered: usize) -> usize {
    let chunk = chunk_bytes.max(1);
    let budget_slots = (max_buffered / chunk).max(1);
    configured.min(budget_slots)
}

// ── Request-size ingress cap ─────────────────────────────────────────

static MAX_REQUEST_BYTES: OnceLock<usize> = OnceLock::new();

/// Maximum accepted request size (header + body) for the **buffered**
/// dispatch entry points, in bytes.  `0` (the default) means
/// **unlimited**, preserving prior behaviour.
///
/// Resolution order (first hit wins, then cached for the process
/// lifetime): [`set_max_request_bytes`] > `VESPERA_MAX_REQUEST_BYTES`
/// env var > `0` (unlimited).
///
/// This is a defense-in-depth ingress cap: a caller that bypasses the
/// autoconfigured Spring proxy (which already routes large bodies to
/// streaming) and feeds a multi-GB body straight into `dispatchBytes` /
/// `dispatchAsync` / `dispatchDirect` would otherwise force a full
/// resident copy.  When set, oversized requests get a `413` wire
/// response **before** the body is dispatched.
///
/// The cap also covers the **response-streaming** entry points
/// (`dispatch_streaming_async`, `dispatch_streaming_with_header_async`)
/// because they still buffer the full *request* in memory — only the
/// *response* is streamed.  **Bidirectional** streaming
/// (`dispatch_bidirectional_streaming*`), which pulls the request body
/// chunk-by-chunk, is intentionally exempt: it is `O(chunk)` RAM and is
/// the correct path for legitimately large payloads.
#[must_use]
#[inline]
pub fn max_request_bytes() -> usize {
    *MAX_REQUEST_BYTES.get_or_init(|| {
        // Absent (or non-Unicode) env → unlimited, the documented default.
        std::env::var("VESPERA_MAX_REQUEST_BYTES")
            .ok()
            .map_or(0, |raw| {
                raw.trim().parse::<usize>().unwrap_or_else(|_| {
                    // Present but unparseable: a typo here (e.g. "10MB", "abc")
                    // would otherwise silently fall through to `0` (unlimited),
                    // disabling the DoS ingress cap with NO signal. Emit a one-time
                    // stderr warning — this `OnceLock` initializer runs at most once
                    // per process — so the misconfiguration is observable, then
                    // preserve the documented unlimited default rather than guessing
                    // an arbitrary numeric cap that could reject legitimate traffic.
                    eprintln!(
                        "vespera: ignoring invalid VESPERA_MAX_REQUEST_BYTES={raw:?} \
                         (expected a non-negative integer in bytes); the request-size \
                         ingress cap stays unlimited"
                    );
                    0
                })
            })
    })
}

/// Override the request-size cap **before the first dispatch**.
/// `0` means unlimited.  Returns `false` when the value was already
/// fixed (a previous call or a dispatch already read it).
pub fn set_max_request_bytes(bytes: usize) -> bool {
    MAX_REQUEST_BYTES.set(bytes).is_ok()
}

/// The single spelling of the ingress-cap predicate: `len` exceeds `max`,
/// where `max == 0` means **unlimited**.
///
/// Pure and `const` so callers that already hold the resolved cap (e.g.
/// [`crate::dispatch::check_ingress_cap`], which loads `max_request_bytes()`
/// once and reuses it for both the test and the `413` message) can share the
/// predicate without paying a second `OnceLock` load. Keeping the
/// security-relevant comparison in one place means a future change to the
/// unlimited sentinel or the strictness of `>` cannot drift between the
/// public helper and the dispatch guard.
///
/// `pub` only because `config` is a **private** module (clippy's
/// `redundant_pub_crate` rejects `pub(crate)` here); it is deliberately
/// absent from the `pub use config::{...}` list in `lib.rs`, so it stays
/// crate-internal and adds nothing to the public API surface.
#[must_use]
#[inline]
pub const fn exceeds(len: usize, max: usize) -> bool {
    max != 0 && len > max
}

/// Whether a request of `len` bytes exceeds the configured cap.
/// Always `false` when the cap is unlimited (`0`).
#[must_use]
#[inline]
pub fn request_exceeds_limit(len: usize) -> bool {
    exceeds(len, max_request_bytes())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_STREAMING_CHANNEL_CAPACITY, DEFAULT_STREAMING_CHUNK_BYTES, parse_config_value,
    };

    #[test]
    fn absent_value_yields_default() {
        assert_eq!(
            parse_config_value(
                "VESPERA_STREAMING_CHUNK_BYTES",
                None,
                DEFAULT_STREAMING_CHUNK_BYTES,
                4096,
                8 << 20
            ),
            DEFAULT_STREAMING_CHUNK_BYTES
        );
    }

    #[test]
    fn unparseable_value_yields_default() {
        for raw in ["", "abc", "-1", "64KiB", "1.5"] {
            assert_eq!(
                parse_config_value(
                    "VESPERA_STREAMING_CHANNEL_CAPACITY",
                    Some(raw),
                    DEFAULT_STREAMING_CHANNEL_CAPACITY,
                    1,
                    1024
                ),
                DEFAULT_STREAMING_CHANNEL_CAPACITY,
                "raw = {raw:?}"
            );
        }
    }

    // The hardcoded `262144` below is the current
    // `DEFAULT_STREAMING_CHUNK_BYTES` (256 KiB).  These tests cover
    // `parse_config_value`'s parsing/clamp behaviour, not the default
    // constant directly — but we keep the representative value in
    // sync with the real default so any future bump only needs one
    // edit per call site.  Bumped from 65536 (64 KiB) when the
    // chunk-size default was raised to 256 KiB for +25 % streaming
    // throughput.
    #[test]
    fn valid_value_is_used_and_whitespace_tolerated() {
        assert_eq!(
            parse_config_value(
                "VESPERA_STREAMING_CHUNK_BYTES",
                Some("131072"),
                262_144,
                4096,
                8 << 20
            ),
            131_072
        );
        assert_eq!(
            parse_config_value(
                "VESPERA_STREAMING_CHANNEL_CAPACITY",
                Some("  64  "),
                16,
                1,
                1024
            ),
            64
        );
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        assert_eq!(
            parse_config_value(
                "VESPERA_STREAMING_CHUNK_BYTES",
                Some("1"),
                262_144,
                4096,
                8 << 20
            ),
            4096
        );
        assert_eq!(
            parse_config_value(
                "VESPERA_STREAMING_CHUNK_BYTES",
                Some("999999999"),
                262_144,
                4096,
                8 << 20
            ),
            8 << 20
        );
    }

    use super::{MAX_STREAMING_BUFFERED_BYTES, cap_channel_slots};

    #[test]
    fn channel_slots_unchanged_when_within_budget() {
        // Default config (256 KiB chunk * 16 slots = 4 MiB) is well under the
        // 64 MiB ceiling, so the configured capacity passes through unchanged.
        assert_eq!(
            cap_channel_slots(16, 256 * 1024, MAX_STREAMING_BUFFERED_BYTES),
            16
        );
    }

    #[test]
    fn channel_slots_capped_for_pathological_max_config() {
        // 8 MiB chunk * 1024 slots would buffer 8 GiB; the product cap clamps
        // the slots to 64 MiB / 8 MiB = 8 (64 MiB peak), not 1024.
        assert_eq!(
            cap_channel_slots(1024, 8 * 1024 * 1024, MAX_STREAMING_BUFFERED_BYTES),
            8
        );
    }

    #[test]
    fn channel_slots_floor_is_one_and_zero_chunk_is_safe() {
        // A chunk larger than the whole budget still yields >= 1 slot so the
        // stream makes progress (peak = one chunk).
        assert_eq!(
            cap_channel_slots(1024, 128 * 1024 * 1024, MAX_STREAMING_BUFFERED_BYTES),
            1
        );
        // Defensive: a 0 chunk size must never divide by zero.
        assert_eq!(cap_channel_slots(16, 0, MAX_STREAMING_BUFFERED_BYTES), 16);
    }

    #[test]
    fn channel_slots_small_chunk_keeps_full_capacity() {
        // 4 KiB chunk * 1024 slots = 4 MiB, under budget → full capacity kept.
        assert_eq!(
            cap_channel_slots(1024, 4 * 1024, MAX_STREAMING_BUFFERED_BYTES),
            1024
        );
    }

    use super::exceeds;
    use rstest::rstest;

    /// [`exceeds`] is the single spelling of the DoS ingress-cap predicate,
    /// shared by [`super::request_exceeds_limit`] and
    /// [`crate::dispatch::check_ingress_cap`] precisely so the two cannot
    /// drift.  Nothing pinned its two security-relevant properties, so these
    /// cases lock them:
    ///
    /// 1. `max == 0` is the **unlimited** sentinel — no `len`, not even
    ///    `usize::MAX`, exceeds it.  Flipping this would turn the documented
    ///    "unlimited by default" into "reject everything".
    /// 2. The comparison is strictly `>`, so a request of exactly `max` bytes
    ///    is **accepted**.  Loosening it to `>=` would silently `413` the
    ///    exact size an operator configured as allowed (an off-by-one the
    ///    end-to-end `tests/request_size_cap.rs` at-cap case also asserts).
    #[rstest]
    // max == 0 (unlimited): every length is accepted.
    #[case::unlimited_empty(0, 0, false)]
    #[case::unlimited_one_byte(1, 0, false)]
    #[case::unlimited_max_len(usize::MAX, 0, false)]
    // Strict `>`: below the cap, exactly at the cap, and an empty body under
    // a finite cap are all accepted.
    #[case::below_cap(9, 10, false)]
    #[case::exactly_at_cap(10, 10, false)]
    #[case::empty_body_finite_cap(0, 10, false)]
    // One byte past the cap is the first rejected size.
    #[case::one_over_cap(11, 10, true)]
    #[case::far_over_cap(usize::MAX, 10, true)]
    // Tightest possible finite cap: 1 byte allowed, 2 rejected.
    #[case::cap_one_at_cap(1, 1, false)]
    #[case::cap_one_over_cap(2, 1, true)]
    fn exceeds_locks_unlimited_sentinel_and_strict_boundary(
        #[case] len: usize,
        #[case] max: usize,
        #[case] expected: bool,
    ) {
        assert_eq!(exceeds(len, max), expected, "exceeds({len}, {max})");
    }
}
