//! Fuzz-style robustness harness for the wire trust boundary.
//!
//! Throws thousands of random, adversarial, and mutated byte sequences
//! at [`vespera_inprocess::dispatch_from_bytes`] and asserts the wire
//! contract on every one:
//!
//! * it **never panics** (no `unwrap`/index/slice/overflow reachable
//!   from hostile input), and
//! * it **always returns a well-formed length-prefixed wire response**
//!   (`[u32 BE header_len | JSON header]`) whose header is valid JSON
//!   carrying a numeric `status`.
//!
//! This is a deterministic (seeded) `cargo test` complement to the
//! coverage-guided `cargo fuzz` target under `fuzz/` (which needs
//! nightly + libFuzzer and runs in CI/Linux).  Any panic prints the
//! offending input prefix for replay.

use std::panic::{AssertUnwindSafe, catch_unwind};

use tokio::runtime::{Builder, Runtime};
use vespera_inprocess::dispatch_from_bytes;

/// Tiny deterministic xorshift PRNG — no dependency, exact replay.
struct XorShift(u64);

impl XorShift {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xff) as u8
    }

    /// Uniform in `[0, n)`; returns 0 when `n == 0`.
    fn range(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        // `v < n` (a `usize`), so it always fits back into `usize`.
        usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
    }
}

/// Dispatch `wire`, asserting no panic and a well-formed wire response.
fn assert_robust(rt: &Runtime, wire: &[u8]) {
    let owned = wire.to_vec();
    let result = catch_unwind(AssertUnwindSafe(|| dispatch_from_bytes(owned, rt)));

    let Ok(resp) = result else {
        let prefix = &wire[..wire.len().min(64)];
        panic!(
            "dispatch_from_bytes PANICKED on input (len={}): {prefix:02x?}",
            wire.len()
        );
    };

    assert!(
        resp.len() >= 4,
        "response shorter than the 4-byte length prefix ({} bytes)",
        resp.len()
    );
    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "response header_len {header_len} overflows response ({} bytes)",
        resp.len()
    );
    let header: serde_json::Value = serde_json::from_slice(&resp[4..4 + header_len])
        .expect("response header must be valid JSON");
    assert!(
        header
            .get("status")
            .and_then(serde_json::Value::as_u64)
            .is_some(),
        "response header must carry a numeric status: {header}"
    );
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime")
}

#[test]
fn random_bytes_never_panic() {
    let rt = runtime();
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for _ in 0..5000 {
        let len = rng.range(512);
        let wire: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        assert_robust(&rt, &wire);
    }
}

#[test]
fn adversarial_header_len_never_panic() {
    let rt = runtime();
    // 4-byte length prefixes claiming huge / edge `header_len` values with
    // varying tails — exercises the bounds checks in `split_wire_request`.
    for header_len in [
        0u32,
        1,
        3,
        4,
        100,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_fffe,
        u32::MAX,
    ] {
        for tail in [0usize, 1, 4, 16, 64] {
            let mut wire = header_len.to_be_bytes().to_vec();
            wire.extend(std::iter::repeat_n(b'{', tail));
            assert_robust(&rt, &wire);
        }
    }
}

#[test]
fn structured_mutation_never_panic() {
    let rt = runtime();
    // Start from a valid wire request and apply random byte mutations /
    // truncations — keeps inputs near the parseable manifold so the
    // deeper header-JSON / body-split paths are exercised, not just the
    // early length-prefix rejects.
    let base = {
        let header = br#"{"v":1,"method":"POST","path":"/x","query":"a=1","headers":{"content-type":"application/json"},"app":"_default"}"#;
        let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
        wire.extend_from_slice(header);
        wire.extend_from_slice(b"{\"k\":\"v\"}");
        wire
    };

    let mut rng = XorShift(0xDEAD_BEEF_CAFE_BABE);
    for _ in 0..3000 {
        let mut wire = base.clone();
        let mutations = 1 + rng.range(4);
        for _ in 0..mutations {
            if wire.is_empty() {
                break;
            }
            let idx = rng.range(wire.len());
            wire[idx] = rng.byte();
        }
        // Occasionally truncate to exercise short/partial inputs.
        if rng.range(3) == 0 && !wire.is_empty() {
            let keep = rng.range(wire.len());
            wire.truncate(keep);
        }
        assert_robust(&rt, &wire);
    }
}
