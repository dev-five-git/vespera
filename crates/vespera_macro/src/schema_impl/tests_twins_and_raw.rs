//! Round-22 regressions: a `#[cfg(..)]`-disabled twin of a serde default
//! function is never read (R22-SC-M1), and a raw-identifier struct is named
//! without its `r#` (R22-SC-L1).

use rstest::rstest;

use super::*;

/// The `count` default of the struct `Page` in `source`, found through the
/// inline module `chain` (empty for the file's top level).
fn page_count_default(source: &str, chain: &[&str]) -> Option<serde_json::Value> {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("twins.rs");
    std::fs::write(&file_path, source).unwrap();
    crate::schema_macro::file_cache::bump_epoch();

    let mut items = syn::parse_file(source).unwrap().items;
    for segment in chain {
        items = items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Mod(module) if module.ident == segment => {
                    module.content.map(|(_, inner)| inner)
                }
                _ => None,
            })
            .unwrap();
    }
    let input: syn::DeriveInput = items
        .into_iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == "Page" => Some(item.into()),
            _ => None,
        })
        .unwrap();
    extract_field_defaults_from_path(&input, &file_path)
        .get("count")
        .cloned()
}

/// R22-SC-M1: the active twin is the one the compiler keeps. The disabled
/// twin comes last in the file (the one the old top-level map kept) or first
/// (the one the old `find_map` kept) and must be skipped either way.
#[rstest]
#[case::top_level_disabled_twin_last(
    "fn limit() -> i64 { 10 }\n#[cfg(any())]\nfn limit() -> i64 { 100 }\n\
     pub struct Page { #[serde(default = \"limit\")] count: i64 }",
    &[],
    Some(serde_json::json!(10))
)]
#[case::inline_module_disabled_twin_first(
    "mod inner {\n#[cfg(any())]\nfn limit() -> i64 { 100 }\nfn limit() -> i64 { 10 }\n\
     pub struct Page { #[serde(default = \"limit\")] count: i64 }\n}",
    &["inner"],
    Some(serde_json::json!(10))
)]
#[case::inherent_method_disabled_twin_first(
    "pub struct Page { #[serde(default = \"Page::limit\")] count: i64 }\n\
     impl Page {\n#[cfg(any())]\nfn limit() -> i64 { 100 }\nfn limit() -> i64 { 10 }\n}",
    &[],
    Some(serde_json::json!(10))
)]
#[case::inherent_block_disabled_twin_first(
    "pub struct Page { #[serde(default = \"Page::limit\")] count: i64 }\n\
     #[cfg(any())]\nimpl Page { fn limit() -> i64 { 100 } }\nimpl Page { fn limit() -> i64 { 10 } }",
    &[],
    Some(serde_json::json!(10))
)]
fn cfg_disabled_default_fn_twin_is_ignored(
    #[case] source: &str,
    #[case] chain: &[&str],
    #[case] expected: Option<serde_json::Value>,
) {
    assert_eq!(page_count_default(source, chain), expected);
}

/// R23-SM3: a platform twin pair (`unix` / `windows`) is left to the
/// compiler, which the static cfg check cannot run, so both survive the
/// filter. The default is then unresolved on EVERY lookup path (it used to be
/// the last twin at the top level and the first one in an inline module).
#[rstest]
#[case::top_level_platform_twins(
    "#[cfg(unix)]\nfn limit() -> i64 { 10 }\n#[cfg(windows)]\nfn limit() -> i64 { 100 }\n\
     pub struct Page { #[serde(default = \"limit\")] count: i64 }",
    &[],
    None
)]
#[case::inline_module_platform_twins(
    "mod inner {\n#[cfg(unix)]\nfn limit() -> i64 { 10 }\n#[cfg(windows)]\nfn limit() -> i64 { 100 }\n\
     pub struct Page { #[serde(default = \"limit\")] count: i64 }\n}",
    &["inner"],
    None
)]
#[case::inherent_method_platform_twins(
    "pub struct Page { #[serde(default = \"Page::limit\")] count: i64 }\n\
     impl Page {\n#[cfg(unix)]\nfn limit() -> i64 { 10 }\n#[cfg(windows)]\nfn limit() -> i64 { 100 }\n}",
    &[],
    None
)]
fn platform_default_fn_twins_are_unresolved(
    #[case] source: &str,
    #[case] chain: &[&str],
    #[case] expected: Option<serde_json::Value>,
) {
    assert_eq!(page_count_default(source, chain), expected);
}

/// R22-SC-L1: `struct r#type` is the component `type`, never `r#type`, and
/// an explicit `#[schema(name = ..)]` keeps the unrawed Rust name as its alias.
#[test]
fn raw_identifier_struct_is_named_without_prefix() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct r#type { x: i32 }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "type");
    assert_eq!(metadata.rust_ident, None);
}

#[test]
fn raw_identifier_struct_with_explicit_name_aliases_unrawed_ident() {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[schema(name = "TypeDto")]
        struct r#type { x: i32 }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "TypeDto");
    assert_eq!(metadata.rust_ident.as_deref(), Some("type"));
}
