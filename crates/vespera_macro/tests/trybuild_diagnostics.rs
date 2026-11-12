//! Compile-fail (UI) tests for the user-facing `vespera_macro` diagnostics.
//!
//! Each case locks the message AND the span of a `syn::Error` the macros emit
//! for misuse, so a regression to a silent drop, a panic, or a span pointing
//! at `call_site` shows up as a `.stderr` diff. The snapshots are
//! toolchain-sensitive; regenerate with:
//!   TRYBUILD=overwrite cargo test -p vespera_macro --test trybuild_diagnostics

#[test]
fn ui_diagnostics() {
    let t = trybuild::TestCases::new();
    // `#[route]`
    t.compile_fail("tests/ui/route_duplicate_args.rs");
    t.compile_fail("tests/ui/route_fn_signature.rs");
    t.compile_fail("tests/ui/route_status_invalid.rs");
    t.compile_fail("tests/ui/route_headers_invalid.rs");
    // `vespera!`
    t.compile_fail("tests/ui/vespera_args_invalid.rs");
    t.compile_fail("tests/ui/vespera_security_servers_invalid.rs");
    // `#[derive(Schema)]`
    t.compile_fail("tests/ui/schema_assertion_missing_custom.rs");
    t.pass("tests/ui/schema_assertion_schema_any.rs");
    t.pass("tests/ui/schema_assertion_serde_json_value.rs");
    t.compile_fail("tests/ui/schema_default_unresolvable.rs");
    t.compile_fail("tests/ui/schema_attr_invalid.rs");
    // `schema!` / `schema_type!`
    t.compile_fail("tests/ui/schema_macro_omit_and_pick.rs");
    t.compile_fail("tests/ui/schema_type_bare_missing.rs");
    t.compile_fail("tests/ui/schema_type_params_invalid.rs");
    t.compile_fail("tests/ui/schema_type_unknown_field.rs");
}
