#[test]
fn ui_diagnostics() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/route_duplicate_args.rs");
}
