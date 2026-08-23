//! Serde attribute extraction utilities for `OpenAPI` schema generation.
//!
//! This module provides functions to extract serde attributes from Rust types
//! to properly generate `OpenAPI` schemas that respect serialization rules.

/// Extract doc comments from attributes.
/// Returns concatenated doc comment string or None if no doc comments.
pub fn extract_doc_comment(attrs: &[syn::Attribute]) -> Option<String> {
    let mut doc_lines = Vec::new();

    for attr in attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(meta_nv) = &attr.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = &meta_nv.value
        {
            let line = lit_str.value();
            // Strip `" / "` or `"/ "` prefixes that can appear when doc-comment
            // markers leak through TokenStream → string → parse roundtrips,
            // then trim any remaining whitespace.
            let trimmed = line
                .strip_prefix(" / ")
                .or_else(|| line.strip_prefix("/ "))
                .unwrap_or(&line)
                .trim();
            doc_lines.push(trimmed.to_string());
        }
    }

    if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    }
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

/// Extract whether `#[serde(transparent)]` is present on a struct.
pub fn extract_transparent(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("serde") {
            return false;
        }

        let mut is_transparent = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("transparent") {
                is_transparent = true;
            }
            Ok(())
        });
        is_transparent
    })
}

/// Extract `#[schema(ref = "Name", nullable)]` override from a struct.
pub fn extract_schema_ref_override(attrs: &[syn::Attribute]) -> Option<(String, bool)> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("schema") {
            return None;
        }

        let mut ref_name = None;
        let mut nullable = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("ref") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                ref_name = Some(lit.value());
            } else if meta.path.is_ident("nullable") {
                nullable = true;
            }
            Ok(())
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

    #[rstest::rstest]
    #[case(" / leaked", "leaked")]
    #[case("/ leaked", "leaked")]
    fn doc_marker_prefixes_are_removed(#[case] input: &str, #[case] expected: &str) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!(r#"#[doc = "{input}"] struct Value;"#)).unwrap();
        assert_eq!(extract_doc_comment(&item.attrs), Some(expected.to_string()));
    }
}
