//! `set_default_multipart_limits` replaces the (total, fields) pair as one:
//! a reader racing it never sees one field of the old pair and one of the
//! new.  Own test binary: it rewrites the process-wide defaults.
#![cfg(feature = "multipart")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vespera::multipart::{MultipartLimits, default_multipart_limits, set_default_multipart_limits};

#[test]
fn concurrent_readers_never_see_a_mixed_pair() {
    // Equal pair before any reader starts: the built-in defaults (64 MiB,
    // 1024 parts) differ, and a reader seeing those is not a torn read.
    set_default_multipart_limits(MultipartLimits::new(1, 1));
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut reads = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let limits = default_multipart_limits();
                    assert_eq!(
                        limits.max_total_bytes, limits.max_fields,
                        "mixed pair observed: {limits:?}"
                    );
                    reads += 1;
                }
                reads
            })
        })
        .collect();
    for round in 0..200_000usize {
        let n = 1 + round % 2;
        let previous = set_default_multipart_limits(MultipartLimits::new(n, n));
        assert_eq!(previous.max_total_bytes, previous.max_fields);
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        assert!(reader.join().expect("reader") > 0);
    }
}
