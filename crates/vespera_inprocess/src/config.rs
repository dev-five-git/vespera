//! Process-wide streaming configuration (chunk size, channel
//! capacity) — resolved once via `OnceLock`: setter > env > default.

use std::sync::OnceLock;

// ── Streaming Configuration ──────────────────────────────────────────

/// Default per-chunk buffer size for streaming dispatches (64 KiB).
///
/// Large enough to amortise per-chunk FFI overhead (JNI region copy +
/// `OutputStream.write` call per chunk), small enough to keep memory
/// bounded for multi-GB streams.
pub const DEFAULT_STREAMING_CHUNK_BYTES: usize = 64 * 1024;

/// Default capacity (slots) of the bounded mpsc channel that feeds
/// request-body chunks into axum during bidirectional streaming.
pub const DEFAULT_STREAMING_CHANNEL_CAPACITY: usize = 16;

const MIN_STREAMING_CHUNK_BYTES: usize = 4 * 1024;
const MAX_STREAMING_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MIN_STREAMING_CHANNEL_CAPACITY: usize = 1;
const MAX_STREAMING_CHANNEL_CAPACITY: usize = 1024;

static STREAMING_CHUNK_BYTES: OnceLock<usize> = OnceLock::new();
static STREAMING_CHANNEL_CAPACITY: OnceLock<usize> = OnceLock::new();

/// Parse an optional config string into a clamped `usize`, falling
/// back to `default` when absent or unparseable.
fn parse_config_value(raw: Option<&str>, default: usize, min: usize, max: usize) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .map_or(default, |v| v.clamp(min, max))
}

/// Effective per-chunk buffer size for streaming dispatches.
///
/// Resolution order (first hit wins, then cached for the process
/// lifetime via `OnceLock` — a single atomic load per call):
///
/// 1. [`set_streaming_chunk_bytes`] called before the first read
/// 2. `VESPERA_STREAMING_CHUNK_BYTES` environment variable
/// 3. [`DEFAULT_STREAMING_CHUNK_BYTES`] (64 KiB)
///
/// Values are clamped to `[4 KiB, 8 MiB]`.
#[must_use]
#[inline]
pub fn streaming_chunk_bytes() -> usize {
    *STREAMING_CHUNK_BYTES.get_or_init(|| {
        parse_config_value(
            std::env::var("VESPERA_STREAMING_CHUNK_BYTES")
                .ok()
                .as_deref(),
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
        parse_config_value(
            std::env::var("VESPERA_STREAMING_CHANNEL_CAPACITY")
                .ok()
                .as_deref(),
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

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_STREAMING_CHANNEL_CAPACITY, DEFAULT_STREAMING_CHUNK_BYTES, parse_config_value,
    };

    #[test]
    fn absent_value_yields_default() {
        assert_eq!(
            parse_config_value(None, DEFAULT_STREAMING_CHUNK_BYTES, 4096, 8 << 20),
            DEFAULT_STREAMING_CHUNK_BYTES
        );
    }

    #[test]
    fn unparseable_value_yields_default() {
        for raw in ["", "abc", "-1", "64KiB", "1.5"] {
            assert_eq!(
                parse_config_value(Some(raw), DEFAULT_STREAMING_CHANNEL_CAPACITY, 1, 1024),
                DEFAULT_STREAMING_CHANNEL_CAPACITY,
                "raw = {raw:?}"
            );
        }
    }

    #[test]
    fn valid_value_is_used_and_whitespace_tolerated() {
        assert_eq!(
            parse_config_value(Some("131072"), 65536, 4096, 8 << 20),
            131_072
        );
        assert_eq!(parse_config_value(Some("  64  "), 16, 1, 1024), 64);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        assert_eq!(parse_config_value(Some("1"), 65536, 4096, 8 << 20), 4096);
        assert_eq!(
            parse_config_value(Some("999999999"), 65536, 4096, 8 << 20),
            8 << 20
        );
    }
}
