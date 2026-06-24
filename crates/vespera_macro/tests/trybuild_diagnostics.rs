#[test]
fn ui_diagnostics() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/route_duplicate_args.rs");
    t.compile_fail("tests/ui/schema_assertion_missing_custom.rs");
    t.pass("tests/ui/schema_assertion_schema_any.rs");
    t.pass("tests/ui/schema_assertion_serde_json_value.rs");
    t.compile_fail("tests/ui/schema_default_unresolvable.rs");
    t.compile_fail("tests/ui/schema_type_bare_missing.rs");
}
