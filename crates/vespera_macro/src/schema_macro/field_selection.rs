//! The fields a `pick` / `omit` name selects, shared by `schema!` and
//! `schema_type!` so both macros read a name the same way.
//!
//! A name selects the field whose Rust identifier it is; `#[cfg]` twins share
//! one identifier and are selected together. Only when no serialized field
//! (one not `#[serde(skip)]` / `skip_serializing`) has that identifier does the
//! name select the serialized fields whose serde JSON name it is: the
//! serialize-side name the generated schema documents. So a serde-skipped
//! field's identifier does not shadow a serialized field's JSON name
//! (R28-SM4-M1: a skipped `id` plus `rename = "id"` on another field selects
//! that field for `omit = ["id"]`, pinned by the `schema!` and `schema_type!`
//! tests). A field a statically false `#[cfg]` (or `cfg_attr(p, cfg(q))`)
//! removes is no candidate, and a
//! JSON name shared by fields with different Rust identifiers is ambiguous
//! (refused).

use std::collections::BTreeSet;
use std::collections::HashSet;

use crate::parser::schema::{
    SerdeSide, derives_serialize, extract_field_rename_owned, extract_serde_rename_all,
    extract_serde_skip_flags,
};
use crate::parser::{extract_rename_all, rename_field, strip_raw_prefix_owned};

use super::cfg_eval::compiled_out;

/// One named field of a source struct a `pick` / `omit` name can select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceField {
    /// Position among the struct's named fields.
    pub index: usize,
    /// The Rust identifier (unraw).
    pub rust: String,
    /// The serde JSON name the schema documents (serialize side).
    pub json: String,
    /// Whether serde writes the field (not `skip` / `skip_serializing`).
    pub serialized: bool,
}

/// The named fields of `struct_item` a `#[cfg]` predicate does not remove, in
/// declaration order, with whether serde writes each one.
pub fn source_fields(struct_item: &syn::ItemStruct) -> Vec<SourceField> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Vec::new();
    };
    // The serialize view: an owner that derives `Serialize` names fields as
    // serde writes them; any other owner (multipart-only) as its `rename_all`.
    let serializes = derives_serialize(&struct_item.attrs);
    let rename_all = if serializes {
        extract_serde_rename_all(&struct_item.attrs)
    } else {
        extract_rename_all(&struct_item.attrs)
    };
    fields_named
        .named
        .iter()
        .enumerate()
        .filter(|(_, field)| !compiled_out(&field.attrs))
        .map(|(index, field)| {
            let rust = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |ident| strip_raw_prefix_owned(ident.to_string()),
            );
            let json = extract_field_rename_owned(&field.attrs, SerdeSide::Serialize, serializes)
                .unwrap_or_else(|| rename_field(&rust, rename_all.as_deref()));
            let flags = extract_serde_skip_flags(&field.attrs);
            SourceField {
                index,
                rust,
                json,
                serialized: !(flags.skip || flags.skip_serializing),
            }
        })
        .collect()
}

/// The fields of `fields` that `name` selects: every field whose Rust
/// identifier it is; only when there is none, every field whose JSON name it is.
pub fn select<'a>(fields: &'a [SourceField], name: &str) -> Vec<&'a SourceField> {
    let by_rust: Vec<&SourceField> = fields.iter().filter(|field| field.rust == name).collect();
    if by_rust.is_empty() {
        fields.iter().filter(|field| field.json == name).collect()
    } else {
        by_rust
    }
}

/// Why a `pick` / `omit` name selects no field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No schema field carries the name. `skipped` is the Rust identifier of a
    /// serde-skipped field the name does select.
    Missing { skipped: Option<String> },
    /// A JSON name shared by fields with different Rust identifiers (sorted).
    Ambiguous(Vec<String>),
}

/// The Rust identifier `name` selects among the schema fields of
/// `struct_item` (the serialized, `cfg`-active ones), or why it is refused.
pub fn resolve_name(struct_item: &syn::ItemStruct, name: &str) -> Result<String, Refusal> {
    let (serialized, skipped): (Vec<SourceField>, Vec<SourceField>) = source_fields(struct_item)
        .into_iter()
        .partition(|field| field.serialized);
    let owners: BTreeSet<String> = select(&serialized, name)
        .into_iter()
        .map(|field| field.rust.clone())
        .collect();
    match owners.len() {
        1 => Ok(owners.into_iter().next().unwrap_or_default()),
        0 => Err(Refusal::Missing {
            skipped: select(&skipped, name)
                .first()
                .map(|field| field.rust.clone()),
        }),
        _ => Err(Refusal::Ambiguous(owners.into_iter().collect())),
    }
}

/// The Rust identifiers `names` select in `struct_item`; a name that selects
/// nothing or is ambiguous is kept as written (the caller refuses it with its
/// own diagnostic).
pub fn selected_rust_names(struct_item: &syn::ItemStruct, names: &[String]) -> HashSet<String> {
    names
        .iter()
        .map(|name| resolve_name(struct_item, name).unwrap_or_else(|_| name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn parse(source: &str) -> syn::ItemStruct {
        syn::parse_str(source).expect("valid struct source")
    }

    #[rstest]
    // The JSON name of a camelCase field selects it (Rust identifier first).
    #[case::camel_json_name(
        "#[derive(serde::Serialize)] #[serde(rename_all = \"camelCase\")] struct S { user_name: String }",
        "userName",
        Ok("user_name")
    )]
    #[case::rust_name_wins_over_json(
        "struct S { #[serde(rename = \"b\")] a: i32, b: i32 }",
        "b",
        Ok("b")
    )]
    #[case::json_name_of_renamed(
        "struct S { #[serde(rename = \"b\")] a: i32, c: i32 }",
        "b",
        Ok("a")
    )]
    #[case::shared_json_name_is_ambiguous(
        "struct S { #[serde(rename = \"v\")] x: i32, #[serde(rename = \"v\")] y: i32 }",
        "v",
        Err(Refusal::Ambiguous(vec!["x".to_string(), "y".to_string()]))
    )]
    #[case::skipped_field_is_reported(
        "struct S { id: i32, #[serde(skip)] hidden: i32 }",
        "hidden",
        Err(Refusal::Missing { skipped: Some("hidden".to_string()) })
    )]
    #[case::skip_serializing_is_reported(
        "struct S { #[serde(skip_serializing)] secret: i32 }",
        "secret",
        Err(Refusal::Missing { skipped: Some("secret".to_string()) })
    )]
    #[case::unknown_name(
        "struct S { id: i32 }",
        "nope",
        Err(Refusal::Missing { skipped: None })
    )]
    #[case::cfg_inactive_field_is_no_candidate(
        "struct S { #[cfg(any())] gone: i32, id: i32 }",
        "gone",
        Err(Refusal::Missing { skipped: None })
    )]
    // R31-SM4-L1 / R31-SM1 note: a field `cfg_attr(p, cfg(q))` removes is no
    // candidate either; a `schema_type!` source is read from its file, where
    // rustc has not stripped it.
    #[case::cfg_attr_inactive_field_is_no_candidate(
        "struct S { #[cfg_attr(all(), cfg(any()))] gone: i32, id: i32 }",
        "gone",
        Err(Refusal::Missing { skipped: None })
    )]
    #[case::cfg_attr_inactive_json_name_is_not_selected(
        "#[derive(serde::Serialize)] struct S { #[cfg_attr(all(), cfg(any()))] #[serde(rename = \"old\")] legacy: i32, id: i32 }",
        "old",
        Err(Refusal::Missing { skipped: None })
    )]
    fn resolves_a_name(
        #[case] source: &str,
        #[case] name: &str,
        #[case] expected: Result<&str, Refusal>,
    ) {
        assert_eq!(
            resolve_name(&parse(source), name),
            expected.map(str::to_string)
        );
    }

    #[test]
    fn cfg_twins_of_one_identifier_resolve_to_it() {
        let source = parse(
            "struct S { #[cfg(all())] #[serde(rename = \"v\")] x: i32, \
             #[cfg(any())] #[serde(rename = \"v\")] y: i32 }",
        );
        assert_eq!(resolve_name(&source, "v"), Ok("x".to_string()));
    }

    #[test]
    fn feature_gated_twin_with_one_json_name_is_not_ambiguous() {
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--crate-name\napp\n--cfg\nfeature=\"a\"",
        );
        let source = parse(
            "struct S { #[cfg(feature = \"a\")] #[serde(rename = \"v\")] x: i32, \
             #[cfg(not(feature = \"a\"))] #[serde(rename = \"v\")] y: i32, other: i32 }",
        );
        assert_eq!(resolve_name(&source, "v"), Ok("x".to_string()));
        assert_eq!(
            resolve_name(&source, "y"),
            Err(Refusal::Missing { skipped: None })
        );
    }

    #[test]
    fn selected_rust_names_keeps_refused_names_as_written() {
        let source = parse("struct S { #[serde(rename = \"v\")] x: i32, id: i32 }");
        let names = ["v".to_string(), "nope".to_string()];
        assert_eq!(
            selected_rust_names(&source, &names),
            HashSet::from(["x".to_string(), "nope".to_string()])
        );
    }
}
