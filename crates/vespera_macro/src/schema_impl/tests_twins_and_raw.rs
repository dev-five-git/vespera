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
                syn::Item::Mod(module)
                    if module.ident == segment
                        && !crate::schema_macro::cfg_eval::compiled_out(&module.attrs) =>
                {
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
/// R25-SM0-M1: a `#[cfg]`-disabled twin MODULE (not a fn) comes first; the
/// function must come from the active `mod inner`, not the disabled one.
#[case::inline_module_disabled_twin_module_first(
    "#[cfg(any())]\nmod inner { pub fn limit() -> i64 { 100 } }\n\
     #[cfg(all())]\nmod inner {\nfn limit() -> i64 { 10 }\n\
     pub struct Page { #[serde(default = \"limit\")] count: i64 }\n}",
    &["inner"],
    Some(serde_json::json!(10))
)]
/// R32-SM0-L1: the same twin pairs gated by `cfg_attr(all(), cfg(any()))`, which
/// rustc drops; a plain `#[cfg]` scan used to take the first (disabled) one.
#[case::top_level_cfg_attr_disabled_twin_first(
    "#[cfg_attr(all(), cfg(any()))]
fn limit() -> i64 { 100 }
fn limit() -> i64 { 10 }
     pub struct Page { #[serde(default = \"limit\")] count: i64 }",
    &[],
    Some(serde_json::json!(10))
)]
#[case::inline_module_cfg_attr_disabled_twin_first(
    "mod inner {
#[cfg_attr(all(), cfg(any()))]
fn limit() -> i64 { 100 }
fn limit() -> i64 { 10 }
     pub struct Page { #[serde(default = \"limit\")] count: i64 }
}",
    &["inner"],
    Some(serde_json::json!(10))
)]
#[case::inherent_block_cfg_attr_disabled_twin_first(
    "pub struct Page { #[serde(default = \"Page::limit\")] count: i64 }
     #[cfg_attr(all(), cfg(any()))]
impl Page { fn limit() -> i64 { 100 } }
impl Page { fn limit() -> i64 { 10 } }",
    &[],
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

/// R32-SM0-L1: the `cfg_attr(p, cfg(q))` twin the compiler keeps follows the
/// feature set of the build: with `legacy` on, the second `limit` is the default.
#[test]
fn cfg_attr_default_fn_twin_follows_the_compiled_feature() {
    let _args = crate::env_access::test_env::set_var(
        crate::env_access::COMPILER_ARGS_KEY,
        "rustc
--crate-name
app
--cfg
feature=\"legacy\"",
    );
    let source = "#[cfg_attr(feature = \"legacy\", cfg(any()))]
fn limit() -> i64 { 100 }
                  #[cfg(feature = \"legacy\")]
fn limit() -> i64 { 10 }
                  pub struct Page { #[serde(default = \"limit\")] count: i64 }";
    assert_eq!(page_count_default(source, &[]), Some(serde_json::json!(10)));
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
