use super::{runtime_worker_threads, set_runtime_worker_threads};

/// One test owns the process-global `OnceLock`: setter wins,
/// clamping applies, and later writes are rejected.
#[test]
fn setter_fixes_clamped_value_first_wins() {
    assert!(set_runtime_worker_threads(99_999), "first set must win");
    assert_eq!(
        runtime_worker_threads(),
        Some(1024),
        "value must clamp to the upper bound"
    );
    assert!(
        !set_runtime_worker_threads(4),
        "second set must be rejected once fixed"
    );
    assert_eq!(runtime_worker_threads(), Some(1024));
}
