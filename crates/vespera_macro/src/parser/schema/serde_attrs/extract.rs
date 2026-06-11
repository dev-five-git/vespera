use super::fallback::{
    contains_standalone_word, quoted_value_after_key, scan_default_from_raw_tokens,
};

pub fn extract_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    // First check serde attrs (higher priority)
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Try using parse_nested_meta for robust parsing
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

            // Fallback: manual token parsing for complex attribute combinations
            let Ok(tokens) = attr.meta.require_list() else {
                continue;
            };
            let token_str = tokens.tokens.to_string();

            if let Some(value) = quoted_value_after_key(&token_str, "rename_all") {
                return Some(value);
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
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename")
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

            // Fallback: manual token parsing for complex attribute combinations
            let tokens = meta_list.tokens.to_string();
            if let Some(value) = quoted_value_after_key(&tokens, "rename") {
                return Some(value);
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
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let tokens = meta_list.tokens.to_string();
            // Check for "skip" (not part of skip_serializing_if or skip_deserializing)
            if tokens.contains("skip") {
                // Make sure it's not skip_serializing_if or skip_deserializing
                if !tokens.contains("skip_serializing_if") && !tokens.contains("skip_deserializing")
                {
                    // Check if it's a standalone "skip"
                    let skip_pos = tokens.find("skip");
                    if let Some(pos) = skip_pos {
                        let before = if pos > 0 { &tokens[..pos] } else { "" };
                        let after = &tokens[pos + "skip".len()..];
                        // Check if skip is not part of another word
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
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("flatten") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: manual token parsing for complex attribute combinations
            if let syn::Meta::List(meta_list) = &attr.meta {
                let tokens = meta_list.tokens.to_string();
                if contains_standalone_word(&tokens, "flatten") {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract `skip_serializing_if` attribute from field attributes
/// Returns true if #[`serde(skip_serializing_if` = "...")] is present
pub fn extract_skip_serializing_if(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip_serializing_if") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: check tokens string for complex attribute combinations
            let tokens = meta_list.tokens.to_string();
            if tokens.contains("skip_serializing_if") {
                return true;
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
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
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
            if found_default.is_none() {
                // Fallback: manual token parsing for complex attribute combinations
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

    // Tests for extract_skip_serializing_if function
    #[rstest]
    #[case(
        r#"#[serde(skip_serializing_if = "Option::is_none")] field: i32"#,
        true
    )]
    #[case(r#"#[serde(skip_serializing_if = "is_zero")] field: i32"#, true)]
    #[case(r"#[serde(default)] field: i32", false)]
    #[case(r"#[serde(skip)] field: i32", false)]
    #[case(r"field: i32", false)]
    fn test_extract_skip_serializing_if(#[case] field_src: &str, #[case] expected: bool) {
        let struct_src = format!("struct Foo {{ {field_src} }}");
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_skip_serializing_if(&field.attrs);
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
}
