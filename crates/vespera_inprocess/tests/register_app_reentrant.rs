//! A router factory that re-enters `register_app*` on the SAME thread must
//! NOT deadlock the non-reentrant registration write lock — it is rejected
//! with an error, and the re-entrancy flag is always cleared (even on a
//! factory panic) so later registrations on that thread still work.

use vespera_inprocess::{Router, try_register_app_named};

/// A factory that re-enters `try_register_app_named` on the same thread is
/// rejected with `Err` instead of deadlocking on the held registration lock.
/// (If the guard regressed, this test would HANG rather than fail.)
#[test]
fn reentrant_registration_returns_err_not_deadlock() {
    let outcome = try_register_app_named("reentrant_outer", || {
        let inner = try_register_app_named("reentrant_inner", Router::new);
        assert!(
            inner.is_err(),
            "re-entrant registration must return Err, got {inner:?}"
        );
        Router::new()
    });
    assert_eq!(outcome, Ok(true), "outer registration should succeed");

    // The inner name was rejected *before* its factory ran, so registering it
    // normally afterwards still succeeds (proves the rejection left no state).
    assert_eq!(
        try_register_app_named("reentrant_inner", Router::new),
        Ok(true)
    );
}

/// A factory panic must clear the re-entrancy flag (via the RAII guard) so the
/// same thread can register again afterwards — it must not be wedged into a
/// permanent "re-entrant" state where every future registration falsely fails.
#[test]
fn factory_panic_clears_reentrancy_flag() {
    // Silence the default panic hook for the intentional panic below.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let panicked = std::panic::catch_unwind(|| {
        let _ = try_register_app_named("panic_app", || -> Router {
            panic!("intentional factory panic");
        });
    });
    std::panic::set_hook(prev);
    assert!(
        panicked.is_err(),
        "factory panic should propagate to the caller"
    );

    // The flag must have been cleared by the RAII guard during unwind, so a
    // subsequent registration on this same thread is NOT falsely rejected.
    assert_eq!(
        try_register_app_named("after_panic_app", Router::new),
        Ok(true)
    );
}
