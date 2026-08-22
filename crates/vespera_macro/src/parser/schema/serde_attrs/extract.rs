use super::fallback::{
    contains_standalone_word, quoted_value_after_key, scan_default_from_raw_tokens,
};

pub fn extract_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    // First check serde attrs (higher priority)
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Try using parse_nested_meta for robust parsing
            let mut found_rename_all = None;
            let parsed = attr.parse_nested_meta(|meta| {
                if meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|seg| seg.ident == "rename_all")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename_all = Some(s.value());
                }
                Ok(())
            });
            if found_rename_all.is_some() {
                return found_rename_all;
            }

            // Fallback ONLY when structured parsing FAILED: a successful
            // parse_nested_meta visited every nested meta, so it cannot have
            // missed a present `rename_all` — skip the throwaway token-string
            // allocation + scan in that (common) case.  An `Err` means an
            // unhandled key/value aborted the walk early (e.g.
            // `skip_serializing_if = "..."` before `rename_all`), which is
            // exactly when the manual scan is still needed.
            if parsed.is_err() {
                let Ok(tokens) = attr.meta.require_list() else {
                    continue;
                };
                let token_str = tokens.tokens.to_string();

                if let Some(value) = quoted_value_after_key(&token_str, "rename_all") {
                    return Some(value);
                }
            }
        }
    }

    // Fallback: check for #[try_from_multipart(rename_all = "...")]
    for attr in attrs {
        if attr.path().is_ident("try_from_multipart") {
            let mut found_rename_all = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename_all")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename_all = Some(s.value());
                }
                Ok(())
            });
            if found_rename_all.is_some() {
                return found_rename_all;
            }
        }
    }

    None
}

pub fn extract_field_rename(attrs: &[syn::Attribute]) -> Option<String> {
    // First check serde attrs (higher priority)
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            // Use parse_nested_meta to parse nested attributes
            let mut found_rename = None;
            let parsed = attr.parse_nested_meta(|meta| {
                if meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|seg| seg.ident == "rename")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename = Some(s.value());
                }
                Ok(())
            });
            if let Some(rename_value) = found_rename {
                return Some(rename_value);
            }

            // Fallback ONLY when structured parsing FAILED (see extract_rename_all):
            // a successful walk cannot have missed a present `rename`, so skip the
            // throwaway token-string allocation + scan in the common case.
            if parsed.is_err() {
                let tokens = meta_list.tokens.to_string();
                if let Some(value) = quoted_value_after_key(&tokens, "rename") {
                    return Some(value);
                }
            }
        }
    }

    // Fallback: check for #[form_data(field_name = "...")]
    for attr in attrs {
        if attr.path().is_ident("form_data") {
            let mut found_field_name = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("field_name")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_field_name = Some(s.value());
                }
                Ok(())
            });
            if found_field_name.is_some() {
                return found_field_name;
            }
        }
    }

    None
}

/// Extract skip attribute from field attributes
/// Returns true if #[serde(skip)] is present
pub fn extract_skip(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut has_skip = false;
            let mut has_skip_serializing = false;
            let mut has_skip_deserializing = false;
            let parsed = attr.parse_nested_meta(|meta| {
                // Match by the path's LAST segment (see extract_flatten) so a
                // qualified `module::skip` is caught by the structured parser,
                // leaving the fallback as a pure parse-error recovery path.
                let last = meta.path.segments.last().map(|seg| &seg.ident);
                if last.is_some_and(|id| id == "skip") {
                    has_skip = true;
                } else if last.is_some_and(|id| id == "skip_serializing") {
                    has_skip_serializing = true;
                } else if last.is_some_and(|id| id == "skip_deserializing") {
                    has_skip_deserializing = true;
                }
                Ok(())
            });
            if has_skip || (has_skip_serializing && has_skip_deserializing) {
                return true;
            }

            // Fallback ONLY when structured parsing FAILED (see extract_rename_all):
            // a successful walk already determined skip is absent, so skip the
            // throwaway token-string allocation + scan in the common case.
            if parsed.is_err() {
                let syn::Meta::List(meta_list) = &attr.meta else {
                    continue;
                };
                let tokens = meta_list.tokens.to_string();
                if contains_standalone_word(&tokens, "skip")
                    || (contains_standalone_word(&tokens, "skip_serializing")
                        && contains_standalone_word(&tokens, "skip_deserializing"))
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract flatten attribute from field attributes
/// Returns true if #[serde(flatten)] is present
pub fn extract_flatten(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Try using parse_nested_meta for robust parsing
            let mut found = false;
            let parsed = attr.parse_nested_meta(|meta| {
                // Match the keyword by the path's LAST segment so a qualified
                // `module::flatten` is recognised by the structured parser
                // itself; the manual fallback below then only covers the genuine
                // parse-error case (an unhandled `key = value` aborting the
                // walk), not "key present but written as a qualified path".
                if meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|seg| seg.ident == "flatten")
                {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback ONLY when structured parsing FAILED (see extract_rename_all):
            // a successful walk already determined flatten is absent, so skip the
            // throwaway token-string allocation + scan in the common case.
            if parsed.is_err()
                && let syn::Meta::List(meta_list) = &attr.meta
            {
                let tokens = meta_list.tokens.to_string();
                if contains_standalone_word(&tokens, "flatten") {
                    return true;
                }
            }
        }
    }
    false
}

/// Check whether the `"default"` substring at index `start` of `tokens`
/// Extract default attribute from field attributes
/// Returns:
/// - Some(None) if #[serde(default)] is present (no function)
/// - `Some(Some(function_name))` if #[serde(default = "`function_name`")] is present
/// - None if no default attribute is present
#[allow(clippy::option_option)]
pub fn extract_default(attrs: &[syn::Attribute]) -> Option<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let mut found_default: Option<Option<String>> = None;
            let parsed = attr.parse_nested_meta(|meta| {
                // Match by the path's LAST segment (see extract_flatten) so a
                // qualified `module::default` is caught by the structured parser.
                if meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|seg| seg.ident == "default")
                {
                    // Check if it has a value (default = "function_name")
                    if let Ok(value) = meta.value() {
                        if let Ok(syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        })) = value.parse::<syn::Expr>()
                        {
                            found_default = Some(Some(s.value()));
                        }
                    } else {
                        // Just "default" without value
                        found_default = Some(None);
                    }
                }
                Ok(())
            });
            // Fallback ONLY when structured parsing FAILED (see extract_rename_all):
            // a successful walk already determined whether `default` is present, so
            // skip the throwaway token-string allocation + scan in the common case.
            if found_default.is_none() && parsed.is_err() {
                found_default = scan_default_from_raw_tokens(&meta_list.tokens.to_string());
            }
            if let Some(default_value) = found_default {
                return Some(default_value);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::option_option)]
    use super::*;
    use rstest::rstest;
    #[rstest]
    #[case(r#"#[serde(rename_all = "camelCase")] struct Foo;"#, Some("camelCase"))]
    #[case(
        r#"#[serde(rename_all = "snake_case")] struct Foo;"#,
        Some("snake_case")
    )]
    #[case(
        r#"#[serde(rename_all = "kebab-case")] struct Foo;"#,
        Some("kebab-case")
    )]
    #[case(
        r#"#[serde(rename_all = "PascalCase")] struct Foo;"#,
        Some("PascalCase")
    )]
    // Multiple attributes - this is the bug case
    #[case(
        r#"#[serde(rename_all = "camelCase", default)] struct Foo;"#,
        Some("camelCase")
    )]
    #[case(
        r#"#[serde(default, rename_all = "snake_case")] struct Foo;"#,
        Some("snake_case")
    )]
    #[case(
    r#"#[serde(rename_all = "kebab-case", skip_serializing_if = "Option::is_none")] struct Foo;"#,
    Some("kebab-case")
)]
    // No rename_all
    #[case(r"#[serde(default)] struct Foo;", None)]
    #[case(r"#[derive(Debug)] struct Foo;", None)]
    fn test_extract_rename_all(#[case] item_src: &str, #[case] expected: Option<&str>) {
        let item: syn::ItemStruct = syn::parse_str(item_src).unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result.as_deref(), expected);
    }

    #[test]
    fn test_extract_rename_all_enum_with_deny_unknown_fields() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            enum Foo { A, B }
        "#,
        )
        .unwrap();
        let result = extract_rename_all(&enum_item.attrs);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    // Tests for extract_field_rename function
    #[rstest]
    #[case(r#"#[serde(rename = "custom_name")] field: i32"#, Some("custom_name"))]
    #[case(r#"#[serde(rename = "userId")] field: i32"#, Some("userId"))]
    #[case(r#"#[serde(rename = "ID")] field: i32"#, Some("ID"))]
    #[case(r"#[serde(default)] field: i32", None)]
    #[case(r"#[serde(skip)] field: i32", None)]
    #[case(r"field: i32", None)]
    // rename_all should NOT be extracted as rename
    #[case(r#"#[serde(rename_all = "camelCase")] field: i32"#, None)]
    // Multiple attributes
    #[case(r#"#[serde(rename = "custom", default)] field: i32"#, Some("custom"))]
    #[case(
        r#"#[serde(default, rename = "my_field")] field: i32"#,
        Some("my_field")
    )]
    fn test_extract_field_rename(#[case] field_src: &str, #[case] expected: Option<&str>) {
        // Parse field from struct context
        let struct_src = format!("struct Foo {{ {field_src} }}");
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_field_rename(&field.attrs);
            assert_eq!(result.as_deref(), expected, "Failed for: {field_src}");
        }
    }

    // Tests for extract_skip function
    #[rstest]
    #[case(r"#[serde(skip)] field: i32", true)]
    #[case(
        r#"#[serde(skip, skip_serializing_if = "Option::is_none")] field: Option<String>"#,
        true
    )]
    #[case(r"#[serde(skip_serializing, skip_deserializing)] field: String", true)]
    #[case(r"#[serde(default)] field: i32", false)]
    #[case(r#"#[serde(rename = "x")] field: i32"#, false)]
    #[case(r"field: i32", false)]
    // skip_serializing_if should NOT be treated as skip
    #[case(
        r#"#[serde(skip_serializing_if = "Option::is_none")] field: i32"#,
        false
    )]
    // skip_deserializing should NOT be treated as skip
    #[case(r"#[serde(skip_deserializing)] field: i32", false)]
    // Combined attributes
    #[case(r"#[serde(skip, default)] field: i32", true)]
    #[case(r"#[serde(default, skip)] field: i32", true)]
    fn test_extract_skip(#[case] field_src: &str, #[case] expected: bool) {
        let struct_src = format!("struct Foo {{ {field_src} }}");
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_skip(&field.attrs);
            assert_eq!(result, expected, "Failed for: {field_src}");
        }
    }

    #[test]
    fn extract_skip_fallback_handles_qualified_key_after_parse_error() {
        use proc_macro2::{Span, TokenStream};

        let tokens: TokenStream = "@broken, module::skip".parse().expect("tokens");
        let attr = syn::Attribute {
            pound_token: syn::token::Pound::default(),
            style: syn::AttrStyle::Outer,
            bracket_token: syn::token::Bracket::default(),
            meta: syn::Meta::List(syn::MetaList {
                path: syn::Path::from(syn::Ident::new("serde", Span::call_site())),
                delimiter: syn::MacroDelimiter::Paren(syn::token::Paren::default()),
                tokens,
            }),
        };
        assert!(extract_skip(&[attr]));
    }

    // Tests for extract_flatten function
    #[rstest]
    #[case(r"#[serde(flatten)] field: i32", true)]
    #[case(r"#[serde(default)] field: i32", false)]
    #[case(r#"#[serde(rename = "x")] field: i32"#, false)]
    #[case(r"field: i32", false)]
    // Combined attributes
    #[case(r"#[serde(flatten, default)] field: i32", true)]
    #[case(r"#[serde(default, flatten)] field: i32", true)]
    fn test_extract_flatten(#[case] field_src: &str, #[case] expected: bool) {
        let struct_src = format!("struct Foo {{ {field_src} }}");
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_flatten(&field.attrs);
            assert_eq!(result, expected, "Failed for: {field_src}");
        }
    }

    // Tests for extract_default function
    #[rstest]
    // Simple default (no function)
    #[case(r"#[serde(default)] field: i32", Some(None))]
    // Default with function name
    #[case(
        r#"#[serde(default = "default_value")] field: i32"#,
        Some(Some("default_value"))
    )]
    #[case(
        r#"#[serde(default = "Default::default")] field: i32"#,
        Some(Some("Default::default"))
    )]
    // No default
    #[case(r"#[serde(skip)] field: i32", None)]
    #[case(r#"#[serde(rename = "x")] field: i32"#, None)]
    #[case(r"field: i32", None)]
    // Combined attributes
    #[case(
        r#"#[serde(default, skip_serializing_if = "Option::is_none")] field: i32"#,
        Some(None)
    )]
    #[case(
        r#"#[serde(skip_serializing_if = "Option::is_none", default = "my_default")] field: i32"#,
        Some(Some("my_default"))
    )]
    fn test_extract_default(
        #[case] field_src: &str,
        #[case]
        #[allow(clippy::option_option)]
        expected: Option<Option<&str>>,
    ) {
        let struct_src = format!("struct Foo {{ {field_src} }}");
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_default(&field.attrs);
            let expected_owned = expected.map(|o| o.map(std::string::ToString::to_string));
            assert_eq!(result, expected_owned, "Failed for: {field_src}");
        }
    }

    #[test]
    fn rename_all_fallback_recovers_after_unconsumed_value() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[serde(@broken, rename_all = "camelCase")]
        };

        assert_eq!(extract_rename_all(&attrs).as_deref(), Some("camelCase"));
    }

    #[test]
    fn multipart_rename_all_is_returned() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[try_from_multipart(rename_all = "snake_case", @broken)]
        };

        assert_eq!(extract_rename_all(&attrs).as_deref(), Some("snake_case"));
    }

    #[test]
    fn field_rename_fallback_recovers_after_unconsumed_value() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[serde(@broken, rename = "userId")]
        };

        assert_eq!(extract_field_rename(&attrs).as_deref(), Some("userId"));
    }

    #[test]
    fn form_data_field_name_is_returned() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[form_data(field_name = "upload", @broken)]
        };

        assert_eq!(extract_field_rename(&attrs).as_deref(), Some("upload"));
    }

    #[test]
    fn skip_fallback_ignores_non_list_then_recovers_from_parse_error() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[serde]
            #[serde(@broken, skip)]
        };

        assert!(extract_skip(&attrs));
    }

    #[test]
    fn flatten_fallback_recovers_after_unconsumed_value() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[serde(@broken, flatten)]
        };

        assert!(extract_flatten(&attrs));
    }
}
