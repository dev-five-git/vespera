/// Serde enum representation types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerdeEnumRepr {
    /// Default externally tagged: `{"VariantName": {...}}`
    ExternallyTagged,
    /// Internally tagged: `{"type": "VariantName", ...fields...}`
    /// Only valid for struct and unit variants
    InternallyTagged { tag: String },
    /// Adjacently tagged: `{"type": "VariantName", "data": {...}}`
    AdjacentlyTagged { tag: String, content: String },
    /// Untagged: `{...fields...}` (no tag, first matching variant wins)
    Untagged,
}

/// Extract serde enum representation from attributes.
///
/// Detects the enum tagging strategy from serde attributes:
/// - `#[serde(tag = "type")]` → `InternallyTagged`
/// - `#[serde(tag = "type", content = "data")]` → `AdjacentlyTagged`
/// - `#[serde(untagged)]` → Untagged
/// - No relevant attributes → `ExternallyTagged` (default)
pub fn extract_enum_repr(attrs: &[syn::Attribute]) -> SerdeEnumRepr {
    let tag = extract_tag(attrs);
    let content = extract_content(attrs);
    let untagged = extract_untagged(attrs);

    if untagged {
        SerdeEnumRepr::Untagged
    } else if let Some(tag_name) = tag {
        if let Some(content_name) = content {
            SerdeEnumRepr::AdjacentlyTagged {
                tag: tag_name,
                content: content_name,
            }
        } else {
            SerdeEnumRepr::InternallyTagged { tag: tag_name }
        }
    } else {
        SerdeEnumRepr::ExternallyTagged
    }
}

/// Extract tag attribute from serde container attributes
/// Returns the tag name if `#[serde(tag = "...")]` is present
pub fn extract_tag(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut found_tag = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("tag")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_tag = Some(s.value());
                }
                Ok(())
            });
            if found_tag.is_some() {
                return found_tag;
            }

            // Fallback: manual token parsing
            let Ok(tokens) = attr.meta.require_list() else {
                continue;
            };
            let token_str = tokens.tokens.to_string();

            if let Some(start) = token_str.find("tag") {
                // Ensure it's "tag" not "untagged"
                let before = if start > 0 { &token_str[..start] } else { "" };
                let before_char = before.chars().last().unwrap_or(' ');
                if before_char != 'n' {
                    // Not "untagged"
                    let remaining = &token_str[start + "tag".len()..];
                    if let Some(equals_pos) = remaining.find('=') {
                        let value_part = remaining[equals_pos + 1..].trim();
                        if let Some(quote_start) = value_part.find('"') {
                            let after_quote = &value_part[quote_start + 1..];
                            if let Some(quote_end) = after_quote.find('"') {
                                let value = &after_quote[..quote_end];
                                return Some(value.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract content attribute from serde container attributes
/// Returns the content name if `#[serde(content = "...")]` is present
pub fn extract_content(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut found_content = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("content")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_content = Some(s.value());
                }
                Ok(())
            });
            if found_content.is_some() {
                return found_content;
            }

            // Fallback: manual token parsing
            let Ok(tokens) = attr.meta.require_list() else {
                continue;
            };
            let token_str = tokens.tokens.to_string();

            if let Some(start) = token_str.find("content") {
                let remaining = &token_str[start + "content".len()..];
                if let Some(equals_pos) = remaining.find('=') {
                    let value_part = remaining[equals_pos + 1..].trim();
                    if let Some(quote_start) = value_part.find('"') {
                        let after_quote = &value_part[quote_start + 1..];
                        if let Some(quote_end) = after_quote.find('"') {
                            let value = &after_quote[..quote_end];
                            return Some(value.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract untagged attribute from serde container attributes
/// Returns true if `#[serde(untagged)]` is present
pub fn extract_untagged(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("untagged") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: manual token parsing
            if let syn::Meta::List(meta_list) = &attr.meta {
                let tokens = meta_list.tokens.to_string();
                if let Some(pos) = tokens.find("untagged") {
                    let before = if pos > 0 { &tokens[..pos] } else { "" };
                    let after = &tokens[pos + "untagged".len()..];
                    let before_char = before.chars().last().unwrap_or(' ');
                    let after_char = after.chars().next().unwrap_or(' ');
                    if (before_char == ' ' || before_char == ',' || before_char == '(')
                        && (after_char == ' ' || after_char == ',' || after_char == ')')
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn get_enum_attrs(serde_content: &str) -> Vec<syn::Attribute> {
        let src = format!(r"#[serde({serde_content})] enum Foo {{ A, B }}");
        let item: syn::ItemEnum = syn::parse_str(&src).unwrap();
        item.attrs
    }

    // extract_tag tests
    #[rstest]
    #[case(r#"tag = "type""#, Some("type"))]
    #[case(r#"tag = "kind""#, Some("kind"))]
    #[case(r#"tag = "variant""#, Some("variant"))]
    #[case(r#"tag = "type", content = "data""#, Some("type"))]
    #[case(r#"rename_all = "camelCase""#, None)]
    #[case(r"untagged", None)]
    #[case(r"default", None)]
    fn test_extract_tag(#[case] serde_content: &str, #[case] expected: Option<&str>) {
        let attrs = get_enum_attrs(serde_content);
        let result = extract_tag(&attrs);
        assert_eq!(result.as_deref(), expected, "Failed for: {serde_content}");
    }

    // extract_content tests
    #[rstest]
    #[case(r#"content = "data""#, Some("data"))]
    #[case(r#"content = "payload""#, Some("payload"))]
    #[case(r#"tag = "type", content = "data""#, Some("data"))]
    #[case(r#"tag = "type""#, None)]
    #[case(r"untagged", None)]
    #[case(r#"rename_all = "camelCase""#, None)]
    fn test_extract_content(#[case] serde_content: &str, #[case] expected: Option<&str>) {
        let attrs = get_enum_attrs(serde_content);
        let result = extract_content(&attrs);
        assert_eq!(result.as_deref(), expected, "Failed for: {serde_content}");
    }

    // extract_untagged tests
    #[rstest]
    #[case(r"untagged", true)]
    #[case(r#"untagged, rename_all = "camelCase""#, true)]
    #[case(r#"rename_all = "camelCase", untagged"#, true)]
    #[case(r#"tag = "type""#, false)]
    #[case(r#"rename_all = "camelCase""#, false)]
    #[case(r"default", false)]
    fn test_extract_untagged(#[case] serde_content: &str, #[case] expected: bool) {
        let attrs = get_enum_attrs(serde_content);
        let result = extract_untagged(&attrs);
        assert_eq!(result, expected, "Failed for: {serde_content}");
    }

    // extract_enum_repr comprehensive tests
    #[test]
    fn test_extract_enum_repr_externally_tagged() {
        // No serde tag attributes - default is externally tagged
        let attrs = get_enum_attrs(r#"rename_all = "camelCase""#);
        let repr = extract_enum_repr(&attrs);
        assert_eq!(repr, SerdeEnumRepr::ExternallyTagged);
    }

    #[test]
    fn test_extract_enum_repr_internally_tagged() {
        let attrs = get_enum_attrs(r#"tag = "type""#);
        let repr = extract_enum_repr(&attrs);
        assert_eq!(
            repr,
            SerdeEnumRepr::InternallyTagged {
                tag: "type".to_string()
            }
        );
    }

    #[test]
    fn test_extract_enum_repr_internally_tagged_custom_name() {
        let attrs = get_enum_attrs(r#"tag = "kind""#);
        let repr = extract_enum_repr(&attrs);
        assert_eq!(
            repr,
            SerdeEnumRepr::InternallyTagged {
                tag: "kind".to_string()
            }
        );
    }

    #[test]
    fn test_extract_enum_repr_adjacently_tagged() {
        let attrs = get_enum_attrs(r#"tag = "type", content = "data""#);
        let repr = extract_enum_repr(&attrs);
        assert_eq!(
            repr,
            SerdeEnumRepr::AdjacentlyTagged {
                tag: "type".to_string(),
                content: "data".to_string()
            }
        );
    }

    #[test]
    fn test_extract_enum_repr_adjacently_tagged_custom_names() {
        let attrs = get_enum_attrs(r#"tag = "kind", content = "payload""#);
        let repr = extract_enum_repr(&attrs);
        assert_eq!(
            repr,
            SerdeEnumRepr::AdjacentlyTagged {
                tag: "kind".to_string(),
                content: "payload".to_string()
            }
        );
    }

    #[test]
    fn test_extract_enum_repr_untagged() {
        let attrs = get_enum_attrs(r"untagged");
        let repr = extract_enum_repr(&attrs);
        assert_eq!(repr, SerdeEnumRepr::Untagged);
    }

    #[test]
    fn test_extract_enum_repr_untagged_with_other_attrs() {
        let attrs = get_enum_attrs(r#"untagged, rename_all = "camelCase""#);
        let repr = extract_enum_repr(&attrs);
        assert_eq!(repr, SerdeEnumRepr::Untagged);
    }

    #[test]
    fn test_extract_enum_repr_no_serde_attrs() {
        let item: syn::ItemEnum = syn::parse_str("enum Foo { A, B }").unwrap();
        let repr = extract_enum_repr(&item.attrs);
        assert_eq!(repr, SerdeEnumRepr::ExternallyTagged);
    }

    // Test that content without tag is still externally tagged (content alone is meaningless)
    #[test]
    fn test_extract_enum_repr_content_without_tag() {
        let attrs = get_enum_attrs(r#"content = "data""#);
        let repr = extract_enum_repr(&attrs);
        // Content without tag should be externally tagged (content is ignored)
        assert_eq!(repr, SerdeEnumRepr::ExternallyTagged);
    }

    // =================================================================
    // FALLBACK PATH TESTS FOR TAG/CONTENT (Lines 573, 583-590, 626)
    // =================================================================

    use proc_macro2::{Span, TokenStream};

    /// Helper to create a serde attribute with raw tokens
    fn create_enum_attr_with_raw_tokens(tokens: TokenStream) -> syn::Attribute {
        syn::Attribute {
            pound_token: syn::token::Pound::default(),
            style: syn::AttrStyle::Outer,
            bracket_token: syn::token::Bracket::default(),
            meta: syn::Meta::List(syn::MetaList {
                path: syn::Path::from(syn::Ident::new("serde", Span::call_site())),
                delimiter: syn::MacroDelimiter::Paren(syn::token::Paren::default()),
                tokens,
            }),
        }
    }

    /// Test extract_tag fallback path - Lines 573, 583-590
    /// Forces manual token parsing by using qualified path
    #[test]
    fn test_extract_tag_fallback_path() {
        let tokens: TokenStream = "my_module::tag = \"type\"".parse().unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_tag(&[attr]);
        assert_eq!(
            result.as_deref(),
            Some("type"),
            "Fallback should extract tag value"
        );
    }

    /// Test extract_tag fallback with complex attributes
    /// Lines 583-590: Tests the value extraction in fallback
    #[test]
    fn test_extract_tag_fallback_complex() {
        let tokens: TokenStream = "crate::tag = \"kind\", rename_all = \"camelCase\""
            .parse()
            .unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_tag(&[attr]);
        assert_eq!(result.as_deref(), Some("kind"));
    }

    /// Test extract_tag fallback doesn't match "untagged"
    /// Line 581: before_char != 'n' check
    #[test]
    fn test_extract_tag_fallback_avoids_untagged() {
        // "untagged" contains "tag" but should not be matched as tag = "..."
        let tokens: TokenStream = "untagged".parse().unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_tag(&[attr]);
        assert_eq!(result, None, "Should not extract tag from 'untagged'");
    }

    /// Test extract_tag fallback with tag after other attributes
    #[test]
    fn test_extract_tag_fallback_at_end() {
        let tokens: TokenStream = "default, some_module::tag = \"variant\"".parse().unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_tag(&[attr]);
        assert_eq!(result.as_deref(), Some("variant"));
    }

    /// Test extract_content fallback path - Line 626
    /// Forces manual token parsing by using qualified path
    #[test]
    fn test_extract_content_fallback_path() {
        let tokens: TokenStream = "my_module::content = \"data\"".parse().unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_content(&[attr]);
        assert_eq!(
            result.as_deref(),
            Some("data"),
            "Fallback should extract content value"
        );
    }

    /// Test extract_content fallback with complex attributes
    /// Line 626+: Tests the fallback token parsing branch
    #[test]
    fn test_extract_content_fallback_complex() {
        let tokens: TokenStream = "crate::tag = \"type\", other::content = \"payload\""
            .parse()
            .unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_content(&[attr]);
        assert_eq!(result.as_deref(), Some("payload"));
    }

    /// Test extract_content fallback with content at different position
    #[test]
    fn test_extract_content_fallback_at_start() {
        let tokens: TokenStream = "some::content = \"body\", tag = \"kind\"".parse().unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let result = extract_content(&[attr]);
        assert_eq!(result.as_deref(), Some("body"));
    }

    /// Test adjacently tagged using fallback paths for both tag and content
    #[test]
    fn test_extract_enum_repr_adjacently_tagged_fallback() {
        let tokens: TokenStream = "mod1::tag = \"type\", mod2::content = \"data\""
            .parse()
            .unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let repr = extract_enum_repr(&[attr]);
        assert_eq!(
            repr,
            SerdeEnumRepr::AdjacentlyTagged {
                tag: "type".to_string(),
                content: "data".to_string()
            }
        );
    }

    /// Test internally tagged using fallback path
    #[test]
    fn test_extract_enum_repr_internally_tagged_fallback() {
        let tokens: TokenStream = "qualified::tag = \"discriminator\"".parse().unwrap();
        let attr = create_enum_attr_with_raw_tokens(tokens);
        let repr = extract_enum_repr(&[attr]);
        assert_eq!(
            repr,
            SerdeEnumRepr::InternallyTagged {
                tag: "discriminator".to_string()
            }
        );
    }

    /// Helper to create a path-only serde attribute (#[serde] without parentheses)
    /// This format causes require_list() to fail (returns Err)
    fn create_path_only_serde_attr() -> syn::Attribute {
        syn::Attribute {
            pound_token: syn::token::Pound::default(),
            style: syn::AttrStyle::Outer,
            bracket_token: syn::token::Bracket::default(),
            meta: syn::Meta::Path(syn::Path::from(syn::Ident::new("serde", Span::call_site()))),
        }
    }

    /// Test extract_tag with non-list serde attribute
    /// When require_list() fails, extract_tag should continue to next attribute
    #[test]
    fn test_extract_tag_non_list_attr_continues() {
        // First attr is path-only (#[serde]), second has the actual tag
        let path_attr = create_path_only_serde_attr();
        let list_attr = {
            let src = r#"#[serde(tag = "type")] enum Foo { A }"#;
            let item: syn::ItemEnum = syn::parse_str(src).unwrap();
            item.attrs.into_iter().next().unwrap()
        };

        // extract_tag should skip the path-only attr and find tag in second attr
        let result = extract_tag(&[path_attr, list_attr]);
        assert_eq!(result.as_deref(), Some("type"));
    }

    /// Test extract_tag with only non-list serde attribute returns None
    #[test]
    fn test_extract_tag_only_non_list_attr_returns_none() {
        let path_attr = create_path_only_serde_attr();
        let result = extract_tag(&[path_attr]);
        assert_eq!(result, None);
    }

    /// Test extract_content with non-list serde attribute
    /// When require_list() fails, extract_content should continue to next attribute
    #[test]
    fn test_extract_content_non_list_attr_continues() {
        // First attr is path-only (#[serde]), second has the actual content
        let path_attr = create_path_only_serde_attr();
        let list_attr = {
            let src = r#"#[serde(content = "data")] enum Foo { A }"#;
            let item: syn::ItemEnum = syn::parse_str(src).unwrap();
            item.attrs.into_iter().next().unwrap()
        };

        // extract_content should skip the path-only attr and find content in second attr
        let result = extract_content(&[path_attr, list_attr]);
        assert_eq!(result.as_deref(), Some("data"));
    }

    /// Test extract_content with only non-list serde attribute returns None
    #[test]
    fn test_extract_content_only_non_list_attr_returns_none() {
        let path_attr = create_path_only_serde_attr();
        let result = extract_content(&[path_attr]);
        assert_eq!(result, None);
    }

    #[test]
    fn untagged_takes_precedence_over_tag_and_content() {
        let attrs = get_enum_attrs(r#"untagged, tag = "type", content = "data""#);
        assert_eq!(extract_enum_repr(&attrs), SerdeEnumRepr::Untagged);
    }

    #[rstest]
    #[case("tag = 42")]
    #[case("tag = true")]
    #[case("tag = value")]
    fn non_string_tag_is_ignored(#[case] tokens: &str) {
        let attr = create_enum_attr_with_raw_tokens(tokens.parse().unwrap());
        assert_eq!(extract_tag(&[attr]), None);
    }

    #[rstest]
    #[case(r#"tag = "type", content = 42"#)]
    #[case(r#"tag = "type", content = true"#)]
    #[case(r#"tag = "type", content = value"#)]
    fn non_string_content_is_ignored(#[case] tokens: &str) {
        let attr = create_enum_attr_with_raw_tokens(tokens.parse().unwrap());
        assert_eq!(extract_content(&[attr]), None);
    }

    #[rstest]
    #[case(r#"qualified::tag = "type""#, Some("type"))]
    #[case("qualified::tag = value", None)]
    #[case("qualified::tag", None)]
    fn malformed_tag_fallback_is_exact(#[case] tokens: &str, #[case] expected: Option<&str>) {
        let attr = create_enum_attr_with_raw_tokens(tokens.parse().unwrap());
        assert_eq!(extract_tag(&[attr]).as_deref(), expected);
    }

    #[rstest]
    #[case(r#"qualified::content = "data""#, Some("data"))]
    #[case("qualified::content = value", None)]
    #[case("qualified::content", None)]
    fn malformed_content_fallback_is_exact(#[case] tokens: &str, #[case] expected: Option<&str>) {
        let attr = create_enum_attr_with_raw_tokens(tokens.parse().unwrap());
        assert_eq!(extract_content(&[attr]).as_deref(), expected);
    }
}
