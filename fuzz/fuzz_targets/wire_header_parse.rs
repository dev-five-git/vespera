#![no_main]
//! Differential fuzz target for the hand-rolled request wire-header parser
//! (`crates/vespera_inprocess/src/wire/header_read.rs`).
//!
//! libFuzzer feeds arbitrary bytes as the header JSON to BOTH parsers via the
//! `#[doc(hidden)]` `bench_support` A/B helpers (compiled under the
//! `bench-support` feature):
//!
//! * `bench_parse_hand`  — the production hand-rolled parser, and
//! * `bench_parse_serde` — the private `serde_json` derive twin it replaced.
//!
//! Each returns `usize::MAX` on reject, otherwise the summed byte length of
//! every decoded field (forcing each `Cow` to materialise — escape decode,
//! UTF-16 surrogates, UTF-8 validation). Contract, any violation aborts and
//! is recorded as a reproducible crash:
//!
//! * neither parser may **panic** on any input;
//! * the hand parser is **never more permissive** than serde (hand accepts
//!   ⇒ serde accepts with the same decoded length);
//! * on any **well-formed JSON document** the two agree exactly.
//!
//! The one tolerated divergence: serde's `ignore_str` (the skip path for an
//! unknown key's value) checks neither UTF-8 nor `\u` surrogate pairing, so
//! `{"…","x":"\udc00"}` is accepted by serde but rejected by the hand
//! parser, whose skip path validates strings exactly like its decode path.
//! Such inputs are not well-formed JSON (`serde_json::Value` rejects them),
//! which is what the tolerance keys on.
//!
//! Run (Linux/macOS, nightly + `cargo install cargo-fuzz`):
//! ```text
//! cargo +nightly fuzz run wire_header_parse
//! ```

use libfuzzer_sys::fuzz_target;
use vespera_inprocess::bench_support::{bench_parse_hand, bench_parse_serde};

fuzz_target!(|data: &[u8]| {
    let hand = bench_parse_hand(data);
    let serde = bench_parse_serde(data);
    if hand == serde {
        return;
    }
    // Hand rejects what serde's lenient unknown-value skip accepted: only
    // allowed when the document is not well-formed JSON at all. (`Value`,
    // not `IgnoredAny`: the latter skips strings through the same lenient
    // `ignore_str`. `Value` also rejects nesting deeper than 128, which only
    // loosens the check.)
    let tolerated =
        hand == usize::MAX && serde_json::from_slice::<serde_json::Value>(data).is_err();
    assert!(
        tolerated,
        "hand-rolled vs serde_json wire-header parse diverged \
         (hand={hand}, serde={serde}; usize::MAX = reject) on {:?}",
        String::from_utf8_lossy(data)
    );
});
