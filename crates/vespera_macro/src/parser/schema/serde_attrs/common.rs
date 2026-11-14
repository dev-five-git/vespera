//! Serde attribute extraction utilities for `OpenAPI` schema generation.
//!
//! This module provides functions to extract serde attributes from Rust types
//! to properly generate `OpenAPI` schemas that respect serialization rules.

use super::extract::{walk_attr_keys, walk_serde_keys};

/// Extract doc comments from attributes.
/// Returns the doc comment text or `None` if there are no doc comments.
///
/// rustdoc semantics: `/// text` is stored as `" text"`, so ONE leading space
/// is removed from every line when all non-blank lines start with one —
/// deeper indentation (indented code blocks, nested Markdown lists) is kept.
/// Trailing whitespace and leading / trailing blank lines are dropped.
pub fn extract_doc_comment(attrs: &[syn::Attribute]) -> Option<String> {
    let mut doc_lines: Vec<String> = Vec::new();

    for attr in attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(meta_nv) = &attr.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = &meta_nv.value
        {
            let value = lit_str.value();
            // Strip `" / "` or `"/ "` prefixes that can appear when doc-comment
            // markers leak through TokenStream → string → parse roundtrips.
            let value = value
                .strip_prefix(" / ")
                .or_else(|| value.strip_prefix("/ "))
                .map_or_else(|| value.clone(), |rest| format!(" {rest}"));
            // A `/** ... */` block arrives as ONE multi-line attribute.
            doc_lines.extend(value.split('\n').map(|line| line.trim_end().to_string()));
        }
    }

    let strip_one = doc_lines
        .iter()
        .filter(|line| !line.is_empty())
        .all(|line| line.starts_with(' '));
    let lines: Vec<&str> = doc_lines
        .iter()
        .map(|line| {
            if strip_one {
                line.strip_prefix(' ').unwrap_or(line)
            } else {
                line.as_str()
            }
        })
        .collect();
    let first = lines.iter().position(|line| !line.is_empty())?;
    let last = lines.iter().rposition(|line| !line.is_empty())?;
    Some(lines[first..=last].join(
        "
",
    ))
}

/// Strips the `r#` prefix from raw identifiers, returning an owned `String`.
/// For the 99% case (no `r#` prefix), returns the input directly with zero extra allocation.
#[allow(clippy::option_if_let_else)] // clippy suggestion doesn't compile: borrow-move conflict
pub fn strip_raw_prefix_owned(ident: String) -> String {
    if let Some(stripped) = ident.strip_prefix("r#") {
        stripped.to_string()
    } else {
        ident
    }
}

pub use crate::schema_macro::type_utils::capitalize_first;

/// Extract a Schema name from a `SeaORM` Entity type path.
///
/// Converts paths like:
/// - `super::user::Entity` -> "User"
/// - `crate::models::memo::Entity` -> "Memo"
///
/// The schema name is derived from the module containing Entity,
/// converted to `PascalCase` (first letter uppercase).
pub fn extract_schema_name_from_entity(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(type_path) => {
            let segments: Vec<_> = type_path.path.segments.iter().collect();

            // Need at least 2 segments: module::Entity
            if segments.len() < 2 {
                return None;
            }

            // Check if last segment is "Entity"
            let last = segments.last()?;
            if last.ident != "Entity" {
                return None;
            }

            // Get the second-to-last segment (module name)
            let module_segment = segments.get(segments.len() - 2)?;
            let module_name = module_segment.ident.to_string();

            // Convert to PascalCase (capitalize first letter)
            // Rust identifiers are guaranteed non-empty, so chars().next() always returns Some
            let schema_name = capitalize_first(&module_name);

            Some(schema_name)
        }
        _ => None,
    }
}

/// Extract whether `#[serde(transparent)]` is present on a struct (in any
/// position: `#[serde(crate = "x", transparent)]` counts too).
pub fn extract_transparent(attrs: &[syn::Attribute]) -> bool {
    let mut found = false;
    walk_serde_keys(attrs, |meta| {
        found |= meta.path.is_ident("transparent");
        Ok(false)
    });
    found
}

/// Extract `#[schema(ref = "Name", nullable)]` override from a struct.
///
/// Unrelated `key = value` entries (`#[schema(name = "X", ref = "Y")]`) are
/// skipped instead of aborting the walk.
pub fn extract_schema_ref_override(attrs: &[syn::Attribute]) -> Option<(String, bool)> {
    attrs.iter().find_map(|attr| {
        let mut ref_name = None;
        let mut nullable = false;
        walk_attr_keys(std::slice::from_ref(attr), "schema", |meta| {
            if meta.path.is_ident("ref") {
                ref_name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                return Ok(true);
            }
            nullable |= meta.path.is_ident("nullable");
            Ok(false)
        });
        ref_name.map(|name| (name, nullable))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Tests for extract_doc_comment function
    #[test]
    fn test_extract_doc_comment_single_line() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[doc = " This is a doc comment"]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, Some("This is a doc comment".to_string()));
    }

    #[test]
    fn test_extract_doc_comment_multi_line() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[doc = " First line"]
            #[doc = " Second line"]
            #[doc = " Third line"]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(
            result,
            Some("First line\nSecond line\nThird line".to_string())
        );
    }

    #[test]
    fn test_extract_doc_comment_no_leading_space() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[doc = "No leading space"]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, Some("No leading space".to_string()));
    }

    /// Only the single space rustdoc inserts after `///` is removed, so
    /// indented Markdown (code blocks, nested lists) survives; blank edges
    /// and trailing whitespace are dropped; `/** */` blocks split into lines.
    #[rstest::rstest]
    #[case::indented_code(
        &[" Example:", "", "     let x = 1;", " - item", "   - nested  "],
        Some("Example:

    let x = 1;
- item
  - nested")
    )]
    #[case::mixed_indentation_keeps_spaces(&["no space", "  two"], Some("no space
  two"))]
    #[case::blank_edges(&["", " body", ""], Some("body"))]
    #[case::only_blank(&["", "   "], None)]
    #[case::block_doc(&["
 Line one
     code
"], Some("Line one
    code"))]
    #[case::leaked_marker(&[" / Leaked"], Some("Leaked"))]
    fn doc_comment_keeps_markdown_indentation(
        #[case] lines: &[&str],
        #[case] expected: Option<&str>,
    ) {
        let attrs: Vec<syn::Attribute> = lines
            .iter()
            .map(|line| syn::parse_quote!(#[doc = #line]))
            .collect();
        assert_eq!(extract_doc_comment(&attrs).as_deref(), expected);
    }

    #[test]
    fn test_extract_doc_comment_empty() {
        let attrs: Vec<syn::Attribute> = vec![];
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_doc_comment_with_non_doc_attrs() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[derive(Debug)]
            #[doc = " The doc comment"]
            #[serde(rename = "test")]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, Some("The doc comment".to_string()));
    }

    // Tests for extract_schema_name_from_entity function
    #[test]
    fn test_extract_schema_name_from_entity_super_path() {
        let ty: syn::Type = syn::parse_str("super::user::Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, Some("User".to_string()));
    }

    #[test]
    fn test_extract_schema_name_from_entity_crate_path() {
        let ty: syn::Type = syn::parse_str("crate::models::memo::Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, Some("Memo".to_string()));
    }

    #[test]
    fn test_extract_schema_name_from_entity_not_entity() {
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_from_entity_single_segment() {
        let ty: syn::Type = syn::parse_str("Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_from_entity_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_from_entity_empty_module_name() {
        // Tests the branch where module name has no characters (edge case)
        let ty: syn::Type = syn::parse_str("super::some_module::Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, Some("Some_module".to_string()));
    }
    /// Test strip_raw_prefix_owned function
    #[test]
    fn test_strip_raw_prefix_owned() {
        assert_eq!(strip_raw_prefix_owned("r#type".to_string()), "type");
        assert_eq!(strip_raw_prefix_owned("r#match".to_string()), "match");
        assert_eq!(strip_raw_prefix_owned("normal".to_string()), "normal");
        assert_eq!(strip_raw_prefix_owned("r#".to_string()), "");
    }

    /// `transparent` is found after any `key = value` / `key(...)` entry.
    #[rstest::rstest]
    #[case::alone("#[serde(transparent)]", true)]
    #[case::after_crate(r#"#[serde(crate = "my_serde", transparent)]"#, true)]
    #[case::after_bound(r#"#[serde(bound(serialize = "T: X"), transparent)]"#, true)]
    #[case::second_attr(r#"#[serde(rename = "W")] #[serde(transparent)]"#, true)]
    #[case::absent(r#"#[serde(rename = "transparent")]"#, false)]
    #[case::other_attr("#[schema(transparent)]", false)]
    fn transparent_after_any_key(#[case] attrs: &str, #[case] expected: bool) {
        let item: syn::ItemStruct = syn::parse_str(&format!("{attrs} struct W(u8);")).unwrap();
        assert_eq!(extract_transparent(&item.attrs), expected);
    }

    #[rstest::rstest]
    #[case::plain(r#"#[schema(ref = "User")]"#, Some(("User", false)))]
    #[case::nullable(r#"#[schema(ref = "User", nullable)]"#, Some(("User", true)))]
    #[case::after_name(r#"#[schema(name = "Dto", ref = "User")]"#, Some(("User", false)))]
    #[case::nullable_first(r#"#[schema(nullable, ref = "User")]"#, Some(("User", true)))]
    #[case::bad_value("#[schema(ref = User)]", None)]
    #[case::none(r#"#[schema(name = "Dto")]"#, None)]
    fn schema_ref_override_after_any_key(
        #[case] attrs: &str,
        #[case] expected: Option<(&str, bool)>,
    ) {
        let item: syn::ItemStruct = syn::parse_str(&format!("{attrs} struct W;")).unwrap();
        assert_eq!(
            extract_schema_ref_override(&item.attrs),
            expected.map(|(name, nullable)| (name.to_string(), nullable))
        );
    }

    #[rstest::rstest]
    #[case(" / leaked", "leaked")]
    #[case("/ leaked", "leaked")]
    fn doc_marker_prefixes_are_removed(#[case] input: &str, #[case] expected: &str) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!(r#"#[doc = "{input}"] struct Value;"#)).unwrap();
        assert_eq!(extract_doc_comment(&item.attrs), Some(expected.to_string()));
    }
}
