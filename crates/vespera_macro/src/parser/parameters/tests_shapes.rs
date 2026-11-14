//! Parameter shapes serde / axum read differently than the declared type
//! suggests: generic `Query` / `Path` structs (type arguments substituted),
//! `Path<Vec<(K, V)>>` pairs, `Path<Struct>` fields matched by alias or
//! through `flatten`, and raw-identifier bindings.

use std::collections::{HashMap, HashSet};

use rstest::rstest;

use super::*;

const DEFINITIONS: &[(&str, &str)] = &[
    (
        "Paged",
        "pub struct Paged<T, S = u32> { pub page: S, #[serde(flatten)] pub filter: T, pub item: Option<T> }",
    ),
    ("Filter", "pub struct Filter { pub q: String }"),
    (
        "Ids",
        r#"pub struct Ids { #[serde(alias = "org")] pub org_id: u64, #[serde(flatten)] pub repo: Option<RepoKey>, #[serde(skip)] pub team: String }"#,
    ),
    ("RepoKey", "pub struct RepoKey { pub repo: i32 }"),
    ("Keyed", "pub struct Keyed<K> { pub key: K }"),
    // Two `#[schema(name)]` renames sharing the ident `Foo` (`a::Foo` = `A`,
    // `b::Foo` = `B`); the bare ident aliases the first.
    (
        "A",
        r#"#[schema(name = "A")] pub struct Foo { pub a_field: i32 }"#,
    ),
    (
        "B",
        r#"#[schema(name = "B")] pub struct Foo { pub b_field: String }"#,
    ),
    (
        "Foo",
        r#"#[schema(name = "A")] pub struct Foo { pub a_field: i32 }"#,
    ),
    (
        "a::Foo",
        r#"#[schema(name = "A")] pub struct Foo { pub a_field: i32 }"#,
    ),
    (
        "b::Foo",
        r#"#[schema(name = "B")] pub struct Foo { pub b_field: String }"#,
    ),
];

/// Every argument of `func_src` parsed with the route's `path_params`,
/// as pretty JSON.
fn render(func_src: &str, path_params: &[&str]) -> String {
    let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
    let known: HashSet<&str> = DEFINITIONS.iter().map(|(name, _)| *name).collect();
    let definitions: HashMap<&str, &str> = DEFINITIONS.iter().copied().collect();
    let path_params: Vec<String> = path_params.iter().map(|name| (*name).to_string()).collect();
    let path_param_set: HashSet<&str> = path_params.iter().map(String::as_str).collect();
    let parameters: Vec<_> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            parse_function_parameter(arg, &path_params, &path_param_set, &known, &definitions)
        })
        .flatten()
        .collect();
    format!(
        "{func_src}\n{}",
        serde_json::to_string_pretty(&parameters).unwrap()
    )
}

#[rstest]
// M21: type arguments reach the generic struct's fields (a flattened `T`,
// a `T`-typed field, and a defaulted parameter).
#[case::query_generic_flatten("query_generic_flatten", "fn f(q: Query<Paged<Filter>>) {}", &[])]
#[case::query_generic_explicit_default(
    "query_generic_explicit_default",
    "fn f(q: Query<Paged<Filter, i64>>) {}",
    &[]
)]
#[case::query_generic_lifetime_argument(
    "query_generic_lifetime_argument",
    "fn f(q: Query<Paged<'static, Filter>>) {}",
    &[]
)]
#[case::query_generic_missing_argument("query_generic_missing_argument", "fn f(q: Query<Paged>) {}", &[])]
#[case::path_generic("path_generic", "fn f(p: Path<Keyed<i64>>) {}", &["key"])]
// L8: `Path<Vec<(String, V)>>` is one `(name, value)` pair per segment.
#[case::path_pairs("path_pairs", "fn f(p: Path<Vec<(String, u32)>>) {}", &["a", "b"])]
// L9: struct fields matched by alias and through `flatten`; skipped fields
// are never read.
#[case::path_alias_flatten("path_alias_flatten", "fn f(p: Path<Ids>) {}", &["org", "repo", "team"])]
// L12: a raw-identifier binding names its segment without `r#`.
#[case::raw_identifier_binding("raw_identifier_binding", "fn f(r#type: Path<u8>) {}", &["type"])]
// M9: a module-qualified extractor type takes the fields of the rename its
// module declares.
#[case::query_scoped_rename("query_scoped_rename", "fn f(q: Query<crate::b::Foo>) {}", &[])]
#[case::path_scoped_rename("path_scoped_rename", "fn f(p: Path<b::Foo>) {}", &["b_field"])]
#[case::raw_identifier_bare("raw_identifier_bare", "fn f(r#type: i32) {}", &["type"])]
fn parameter_shapes(#[case] name: &str, #[case] func_src: &str, #[case] path_params: &[&str]) {
    insta::assert_snapshot!(
        format!("parameter_shapes_{name}"),
        render(func_src, path_params)
    );
}
