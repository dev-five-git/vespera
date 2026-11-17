//! `requested_variant` (R20-SM3-03, round 20): a bare name the call site
//! imports explicitly names that import's module; a glob import does not.

use super::*;
use crate::metadata::StructMetadata;

/// A crate with `a::Page` (no `#[derive(Schema)]`), `b::Page` (the stored,
/// derived one) and `routes` holding `routes_source` as its only `use`.
fn write_crate(dir: &std::path::Path, routes_source: &str) -> String {
    let files = [
        ("src/lib.rs", "pub mod a;\npub mod b;\npub mod routes;\n"),
        ("src/a.rs", "pub struct Page { pub x: i32 }\n"),
        ("src/b.rs", "pub struct Page { pub y: i32 }\n"),
        ("src/routes.rs", routes_source),
    ];
    for (path, content) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir.join("src/b.rs").display().to_string()
}

#[rstest::rstest]
#[case::explicit_import_of_an_underived_item("use crate::a::Page;\n", true)]
#[case::glob_import_stays_lenient("use crate::a::*;\n", false)]
fn bare_name_import_of_an_underived_item(#[case] routes_source: &str, #[case] rejected: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let stored_file = write_crate(dir.path(), routes_source);
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    let _call_site = set_test_call_site_module("crate::routes");
    let stored = StructMetadata::new(
        "Page".to_string(),
        "pub struct Page { pub y: i32 }".to_string(),
    )
    .with_source_identity(format!("{stored_file}::Page@1:1"));
    let ty: syn::Type = syn::parse_quote!(Page);
    let result = requested_variant(&ty, &stored);
    assert_eq!(result.is_err(), rejected);
}
