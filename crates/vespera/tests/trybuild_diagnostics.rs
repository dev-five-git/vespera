//! Compile-fail (UI) tests for the macro diagnostics: malformed
//! `#[route(responses = [...])]` and `#[cron("...")]` input must fail at
//! COMPILE time with a clear message — instead of being silently dropped
//! (incomplete OpenAPI) or panicking the `JobScheduler` at application startup.
//!
//! The `.stderr` snapshots are toolchain-sensitive; regenerate with:
//!   TRYBUILD=overwrite cargo test -p vespera --features cron --test trybuild_diagnostics

#[test]
fn ui_diagnostics() {
    let t = trybuild::TestCases::new();
    // `responses` validation lives in `RouteArgs::parse` (always compiled).
    t.compile_fail("tests/ui/route_responses_invalid.rs");
    // The cron-syntax validator only compiles into the proc-macro under the
    // `cron` feature (enabled transitively by `vespera`'s `cron` feature), so
    // only assert the cron diagnostic when that feature is on.
    #[cfg(feature = "cron")]
    t.compile_fail("tests/ui/cron_invalid.rs");
}
