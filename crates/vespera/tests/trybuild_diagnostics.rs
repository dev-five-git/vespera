//! Compile-fail (UI) tests for the macro diagnostics: malformed
//! `#[route(responses = [...])]` and `#[cron("...")]` input — and a
//! `Validated<Json<T>>` payload without `garde::Validate` — must fail at
//! COMPILE time with a clear message — instead of being silently dropped
//! (incomplete OpenAPI) or panicking the `JobScheduler` at application startup.
//!
//! The `.stderr` snapshots are toolchain-sensitive; regenerate with:
//!   TRYBUILD=overwrite cargo test -p vespera --features cron --test trybuild_diagnostics
//!   TRYBUILD=overwrite cargo test -p vespera --features cron-tz --test trybuild_diagnostics
//!   TRYBUILD=overwrite cargo test -p vespera --no-default-features --test trybuild_diagnostics

#[test]
fn ui_diagnostics() {
    let t = trybuild::TestCases::new();
    // `responses` validation lives in `RouteArgs::parse` (always compiled).
    t.compile_fail("tests/ui/route_responses_invalid.rs");
    // A `responses` type without `#[derive(Schema)]` (dangling `$ref`).
    t.compile_fail("tests/ui/route_responses_not_schema.rs");
    // The cron-syntax validator only compiles into the proc-macro under the
    // `cron` feature (enabled transitively by `vespera`'s `cron` feature), so
    // only assert the cron diagnostic when that feature is on.
    #[cfg(feature = "cron")]
    t.compile_fail("tests/ui/cron_invalid.rs");
    // `overlap` values are checked by the attribute parser itself.
    t.compile_fail("tests/ui/cron_invalid_overlap.rs");
    // `tz` names are looked up in the IANA database under `cron-tz`, and the
    // argument is rejected (naming the feature) without it.
    #[cfg(feature = "cron-tz")]
    t.compile_fail("tests/ui/cron_invalid_tz.rs");
    #[cfg(all(feature = "cron", not(feature = "cron-tz")))]
    t.compile_fail("tests/ui/cron_tz_needs_feature.rs");
    // A `Validated` payload without `garde::Validate` (rule-less struct, enum)
    // reports `AssertValidate`'s `#[diagnostic::on_unimplemented]` message.
    #[cfg(feature = "validation")]
    t.compile_fail("tests/ui/validated_payload_without_validate.rs");
    // Without the (default-on) `multipart` feature, `#[derive(Multipart)]`
    // resolves to a stand-in derive whose `compile_error!` names the feature.
    #[cfg(not(feature = "multipart"))]
    t.compile_fail("tests/ui/multipart_needs_feature.rs");
}
