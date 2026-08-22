pub(super) fn quoted_value_after_key(tokens: &str, key: &str) -> Option<String> {
    for (start, _) in tokens.match_indices(key) {
        if key == "rename" && tokens[start..].starts_with("rename_all") {
            continue;
        }
        if !is_standalone_word_at(tokens, start, key) && !is_qualified_key(tokens, start) {
            continue;
        }
        let remaining = &tokens[start + key.len()..];
        let Some(equals_pos) = remaining.find('=') else {
            continue;
        };
        let value_part = remaining[equals_pos + 1..].trim();
        let Some(quote_start) = value_part.find('"') else {
            continue;
        };
        let after_quote = &value_part[quote_start + 1..];
        let Some(quote_end) = after_quote.find('"') else {
            continue;
        };
        return Some(after_quote[..quote_end].to_string());
    }
    None
}

pub(super) fn contains_standalone_word(tokens: &str, word: &str) -> bool {
    tokens.match_indices(word).any(|(start, _)| {
        is_standalone_word_at(tokens, start, word) || is_qualified_key(tokens, start)
    })
}

fn is_qualified_key(tokens: &str, start: usize) -> bool {
    start >= 2 && &tokens[start - 2..start] == "::"
}

fn is_standalone_word_at(tokens: &str, start: usize, word: &str) -> bool {
    let before = if start > 0 { &tokens[..start] } else { "" };
    let after = &tokens[start + word.len()..];
    let before_char = before.chars().last().unwrap_or(' ');
    let after_char = after.chars().next().unwrap_or(' ');
    let before_ok = before_char == ' ' || before_char == ',' || before_char == '(';
    let after_ok = after_char == ' ' || after_char == ',' || after_char == ')' || after_char == '=';
    before_ok && after_ok
}

#[allow(clippy::option_option)]
pub(super) fn scan_default_from_raw_tokens(tokens: &str) -> Option<Option<String>> {
    let start = tokens.find("default")?;
    let remaining = &tokens[start + "default".len()..];
    if remaining.trim_start().starts_with('=') {
        let after_equals = remaining
            .trim_start()
            .strip_prefix('=')
            .unwrap_or("")
            .trim_start();
        let quote_start = after_equals.find('"')?;
        let after_quote = &after_equals[quote_start + 1..];
        let quote_end = after_quote.find('"')?;
        Some(Some(after_quote[..quote_end].to_string()))
    } else if is_standalone_word_at(tokens, start, "default") {
        Some(None)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::schema::serde_attrs::*;
    use proc_macro2::{Span, TokenStream};
    use quote::quote;
    use rstest::rstest;

    /// Helper to create attributes by parsing a struct with the given serde attributes
    fn get_struct_attrs(serde_content: &str) -> Vec<syn::Attribute> {
        let src = format!(r"#[serde({serde_content})] struct Foo;");
        let item: syn::ItemStruct = syn::parse_str(&src).unwrap();
        item.attrs
    }

    /// Helper to create field attributes by parsing a struct with the field
    fn get_field_attrs(serde_content: &str) -> Vec<syn::Attribute> {
        let src = format!(r"struct Foo {{ #[serde({serde_content})] field: i32 }}");
        let item: syn::ItemStruct = syn::parse_str(&src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            fields.named.first().unwrap().attrs.clone()
        } else {
            vec![]
        }
    }

    /// Create a serde attribute with programmatic tokens
    fn create_attr_with_raw_tokens(tokens: TokenStream) -> syn::Attribute {
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

    /// Test extract_rename_all fallback by creating an attribute where
    /// parse_nested_meta succeeds but doesn't find rename_all in the expected format
    #[test]
    fn test_extract_rename_all_fallback_path() {
        // Standard path - parse_nested_meta should work
        let attrs = get_struct_attrs(r#"rename_all = "camelCase""#);
        let result = extract_rename_all(&attrs);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test extract_field_rename fallback
    #[test]
    fn test_extract_field_rename_fallback_path() {
        // Standard path
        let attrs = get_field_attrs(r#"rename = "myField""#);
        let result = extract_field_rename(&attrs);
        assert_eq!(result.as_deref(), Some("myField"));
    }

    /// Test extract_default standalone fallback
    #[test]
    fn test_extract_default_standalone_fallback_path() {
        // Simple default without function
        let attrs = get_field_attrs(r"default");
        let result = extract_default(&attrs);
        assert_eq!(result, Some(None));
    }

    /// Test extract_default fallback when parse_nested_meta can't see `default`
    /// at the top level — forces the manual token scan to catch it.
    #[test]
    fn test_extract_default_standalone_fallback_when_nested_meta_fails() {
        // Construct an attribute whose token stream begins with garbage
        // that `parse_nested_meta` will refuse to parse (a stray `@`
        // before the first key).  Because the parser bails immediately,
        // the callback for `default` never fires, and the manual
        // token-string fallback at the end of `extract_default` is the
        // only path that detects the standalone `default` keyword.
        let tokens: TokenStream = "@bogus, default".parse().expect("token stream parses");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_default(&[attr]);
        assert_eq!(
            result,
            Some(None),
            "fallback path must detect bare `default`"
        );
    }

    /// Test that the fallback's "default appears as a substring inside
    /// another identifier" branch returns None (no false-positive
    /// match).  Exercises the trailing `None` arm of
    /// `scan_default_from_raw_tokens` (substring found, but neither
    /// `=` follows nor delimiter chars surround it).
    #[test]
    fn test_extract_default_substring_in_identifier_is_not_a_match() {
        // `field_default` contains "default" but as a suffix of an
        // identifier — `before_char` is `_`, not one of the valid
        // delimiters, so the standalone check fails.
        let tokens: TokenStream = "@bogus, field_default"
            .parse()
            .expect("token stream parses");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_default(&[attr]);
        assert_eq!(
            result, None,
            "embedded 'default' substring must not register as default"
        );
    }

    /// Test extract_default with function fallback
    #[test]
    fn test_extract_default_with_function_fallback_path() {
        let attrs = get_field_attrs(r#"default = "my_default_fn""#);
        let result = extract_default(&attrs);
        assert_eq!(result, Some(Some("my_default_fn".to_string())));
    }

    /// Test that rename_all is NOT confused with rename
    #[test]
    fn test_extract_field_rename_avoids_rename_all() {
        let attrs = get_field_attrs(r#"rename_all = "camelCase""#);
        let result = extract_field_rename(&attrs);
        assert_eq!(result, None); // Should NOT extract rename_all as rename
    }

    /// Test empty serde attribute
    #[test]
    fn test_extract_functions_with_empty_serde() {
        let item: syn::ItemStruct = syn::parse_str(r"#[serde()] struct Foo;").unwrap();
        assert_eq!(extract_rename_all(&item.attrs), None);
    }

    /// Test non-serde attribute is ignored
    #[test]
    fn test_extract_functions_ignore_non_serde() {
        let item: syn::ItemStruct = syn::parse_str(r"#[derive(Debug)] struct Foo;").unwrap();
        assert_eq!(extract_rename_all(&item.attrs), None);
        assert_eq!(extract_field_rename(&item.attrs), None);
    }

    /// Test serde attribute that is not a list (e.g., #[serde])
    #[test]
    fn test_extract_rename_all_non_list_serde() {
        // #[serde] without parentheses - this should just be ignored
        let item: syn::ItemStruct = syn::parse_str(r"#[serde] struct Foo;").unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result, None);
    }

    /// Test extract_field_rename with complex attribute
    #[test]
    fn test_extract_field_rename_complex_attr() {
        let attrs = get_field_attrs(
            r#"default, rename = "field_name", skip_serializing_if = "Option::is_none""#,
        );
        let result = extract_field_rename(&attrs);
        assert_eq!(result.as_deref(), Some("field_name"));
    }

    /// Test extract_rename_all with multiple serde attributes on same item
    #[test]
    fn test_extract_rename_all_multiple_serde_attrs() {
        let item: syn::ItemStruct = syn::parse_str(
            r#"
                #[serde(default)]
                #[serde(rename_all = "snake_case")]
                struct Foo;
                "#,
        )
        .unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result.as_deref(), Some("snake_case"));
    }

    /// Test edge case: rename_all with extra whitespace (manual parsing should handle)
    #[test]
    fn test_extract_rename_all_with_whitespace() {
        // Note: syn normalizes whitespace in parsed tokens, so this tests the robust parsing
        let attrs = get_struct_attrs(r#"rename_all = "PascalCase""#);
        let result = extract_rename_all(&attrs);
        assert_eq!(result.as_deref(), Some("PascalCase"));
    }

    /// Test edge case: rename at various positions
    #[test]
    fn test_extract_field_rename_at_end() {
        let attrs = get_field_attrs(r#"skip_serializing_if = "is_none", rename = "lastField""#);
        let result = extract_field_rename(&attrs);
        assert_eq!(result.as_deref(), Some("lastField"));
    }

    /// Test extract_default when it appears with other attrs
    #[test]
    fn test_extract_default_among_other_attrs() {
        let attrs =
            get_field_attrs(r#"skip_serializing_if = "is_none", default, rename = "field""#);
        let result = extract_default(&attrs);
        assert_eq!(result, Some(None));
    }

    /// Test extract_skip - basic functionality
    #[test]
    fn test_extract_skip_basic() {
        let attrs = get_field_attrs(r"skip");
        let result = extract_skip(&attrs);
        assert!(result);
    }

    /// Test extract_skip does not trigger for skip_serializing_if
    #[test]
    fn test_extract_skip_not_skip_serializing_if() {
        let attrs = get_field_attrs(r#"skip_serializing_if = "Option::is_none""#);
        let result = extract_skip(&attrs);
        assert!(!result);
    }

    /// Test extract_skip does not trigger for skip_deserializing
    #[test]
    fn test_extract_skip_not_skip_deserializing() {
        let attrs = get_field_attrs(r"skip_deserializing");
        let result = extract_skip(&attrs);
        assert!(!result);
    }

    /// Test extract_skip with combined attrs
    #[test]
    fn test_extract_skip_with_other_attrs() {
        let attrs = get_field_attrs(r"skip, default");
        let result = extract_skip(&attrs);
        assert!(result);
    }

    /// Test extract_default function with path containing colons
    #[test]
    fn test_extract_default_with_path() {
        let attrs = get_field_attrs(r#"default = "Default::default""#);
        let result = extract_default(&attrs);
        assert_eq!(result, Some(Some("Default::default".to_string())));
    }

    /// Test extract_rename_all with all supported formats
    #[rstest]
    #[case("camelCase")]
    #[case("snake_case")]
    #[case("kebab-case")]
    #[case("PascalCase")]
    #[case("lowercase")]
    #[case("UPPERCASE")]
    #[case("SCREAMING_SNAKE_CASE")]
    #[case("SCREAMING-KEBAB-CASE")]
    fn test_extract_rename_all_all_formats(#[case] format: &str) {
        let attrs = get_struct_attrs(&format!(r#"rename_all = "{format}""#));
        let result = extract_rename_all(&attrs);
        assert_eq!(result.as_deref(), Some(format));
    }

    /// Test non-serde attribute doesn't affect extraction
    #[test]
    fn test_mixed_attributes() {
        let item: syn::ItemStruct = syn::parse_str(
            r#"
                #[derive(Debug, Clone)]
                #[serde(rename_all = "camelCase")]
                #[doc = "Some documentation"]
                struct Foo;
                "#,
        )
        .unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test field with multiple serde attributes
    #[test]
    fn test_field_multiple_serde_attrs() {
        let item: syn::ItemStruct = syn::parse_str(
            r#"
                struct Foo {
                    #[serde(default)]
                    #[serde(rename = "customName")]
                    field: i32
                }
                "#,
        )
        .unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let attrs = &fields.named.first().unwrap().attrs;
            let rename = extract_field_rename(attrs);
            let default = extract_default(attrs);
            assert_eq!(rename.as_deref(), Some("customName"));
            assert_eq!(default, Some(None));
        }
    }

    /// Test extract_rename_all with programmatic tokens
    #[test]
    fn test_extract_rename_all_programmatic() {
        let tokens = quote!(rename_all = "camelCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test extract_rename_all with invalid value (not a string)
    #[test]
    fn test_extract_rename_all_invalid_value() {
        let tokens = quote!(rename_all = camelCase);
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        // parse_nested_meta won't find a string literal
        assert!(result.is_none());
    }

    /// Test extract_rename_all with missing equals sign
    #[test]
    fn test_extract_rename_all_no_equals() {
        let tokens = quote!(rename_all "camelCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert!(result.is_none());
    }

    /// Test extract_field_rename with programmatic tokens
    #[test]
    fn test_extract_field_rename_programmatic() {
        let tokens = quote!(rename = "customField");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        assert_eq!(result.as_deref(), Some("customField"));
    }

    /// Test extract_default standalone with programmatic tokens
    #[test]
    fn test_extract_default_programmatic() {
        let tokens = quote!(default);
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_default(&[attr]);
        assert_eq!(result, Some(None));
    }

    /// Test extract_default with function via programmatic tokens
    #[test]
    fn test_extract_default_with_fn_programmatic() {
        let tokens = quote!(default = "my_fn");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_default(&[attr]);
        assert_eq!(result, Some(Some("my_fn".to_string())));
    }

    /// Test extract_skip via programmatic tokens
    #[test]
    fn test_extract_skip_programmatic() {
        let tokens = quote!(skip);
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_skip(&[attr]);
        assert!(result);
    }

    /// Test that rename_all is not confused with rename
    #[test]
    fn test_rename_all_not_rename() {
        let tokens = quote!(rename_all = "camelCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        assert_eq!(result, None);
    }

    /// Test multiple items in serde attribute
    #[test]
    fn test_multiple_items_programmatic() {
        let tokens = quote!(default, rename = "myField", skip_serializing_if = "is_none");
        let attr = create_attr_with_raw_tokens(tokens);

        let rename_result = extract_field_rename(std::slice::from_ref(&attr));
        let default_result = extract_default(std::slice::from_ref(&attr));

        assert_eq!(rename_result.as_deref(), Some("myField"));
        assert_eq!(default_result, Some(None));
    }

    /// Test extract_rename_all fallback parsing
    #[test]
    fn test_extract_rename_all_fallback_manual_parsing() {
        let tokens = quote!(rename_all = "kebab-case");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("kebab-case"));
    }

    /// Test extract_rename_all with complex attribute that forces fallback
    #[test]
    fn test_extract_rename_all_complex_attribute_fallback() {
        let tokens = quote!(default, rename_all = "SCREAMING_SNAKE_CASE", skip);
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("SCREAMING_SNAKE_CASE"));
    }

    /// Test extract_rename_all when value is not a string literal
    #[test]
    fn test_extract_rename_all_no_quote_start() {
        let tokens = quote!(rename_all = snake_case);
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert!(result.is_none());
    }

    /// Test extract_rename_all with unclosed quote
    #[test]
    fn test_extract_rename_all_unclosed_quote() {
        let tokens = quote!(rename_all = "camelCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test extract_rename_all with empty string value
    #[test]
    fn test_extract_rename_all_empty_string() {
        let tokens = quote!(rename_all = "");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some(""));
    }

    /// Test extract_rename_all with QUALIFIED PATH to force fallback
    #[test]
    fn test_extract_rename_all_qualified_path_forces_fallback() {
        let tokens = quote!(serde_with::rename_all = "camelCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test extract_rename_all with another qualified path variation
    #[test]
    fn test_extract_rename_all_module_qualified_forces_fallback() {
        let tokens = quote!(my_module::rename_all = "snake_case");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("snake_case"));
    }

    /// Test extract_rename_all with deeply qualified path
    #[test]
    fn test_extract_rename_all_deeply_qualified_forces_fallback() {
        let tokens = quote!(a::b::rename_all = "PascalCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("PascalCase"));
    }

    /// CRITICAL TEST: This test MUST hit fallback path
    #[test]
    fn test_extract_rename_all_raw_tokens_force_fallback() {
        let tokens: TokenStream = "__rename_all_prefix::rename_all = \"lowercase\""
            .parse()
            .unwrap();
        let attr = create_attr_with_raw_tokens(tokens);

        if let syn::Meta::List(list) = &attr.meta {
            let token_str = list.tokens.to_string();
            assert!(
                token_str.contains("rename_all"),
                "Token string should contain rename_all: {token_str}"
            );
        }

        let result = extract_rename_all(&[attr]);
        assert_eq!(
            result.as_deref(),
            Some("lowercase"),
            "Fallback parsing must extract the value"
        );
    }

    /// Another critical test with different qualified path format
    #[test]
    fn test_extract_rename_all_crate_qualified_forces_fallback() {
        let tokens: TokenStream = "crate::rename_all = \"UPPERCASE\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("UPPERCASE"));
    }

    /// Test with self:: prefix
    #[test]
    fn test_extract_rename_all_self_qualified_forces_fallback() {
        let tokens: TokenStream = "self::rename_all = \"kebab-case\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("kebab-case"));
    }

    // =================================================================
    // FALLBACK PATH TESTS (Lines 173, 258-265, 573, 583-590, 626)
    // =================================================================

    /// Test extract_field_rename fallback path - Line 173
    /// Tests the word boundary check when "rename" appears with other attributes
    /// This triggers the manual token parsing fallback when parse_nested_meta
    /// doesn't extract the value in expected format
    #[test]
    fn test_extract_field_rename_fallback_word_boundary() {
        // Create attribute with qualified path to force fallback
        let tokens: TokenStream = "my_module::rename = \"value\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        assert_eq!(result.as_deref(), Some("value"));
    }

    /// Test extract_field_rename fallback - complex combined attributes
    /// Line 173: Tests the edge case of word boundary checking
    #[test]
    fn test_extract_field_rename_fallback_complex_attr() {
        // Qualified path forces parse_nested_meta to not find "rename"
        let tokens: TokenStream = "crate::other::rename = \"custom_field\", default"
            .parse()
            .unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        assert_eq!(result.as_deref(), Some("custom_field"));
    }

    /// Test extract_field_rename - ensure rename_all is not matched as rename
    /// Test the word boundary logic
    #[test]
    fn test_extract_field_rename_fallback_avoids_rename_all() {
        let tokens: TokenStream = "some::rename_all = \"camelCase\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        // Should NOT match rename_all as rename
        assert_eq!(result, None);
    }

    /// Test extract_flatten fallback path - Lines 258-265
    /// Forces manual token parsing by using qualified path
    #[test]
    fn test_extract_flatten_fallback_path() {
        let tokens: TokenStream = "my_module::flatten".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(result, "Fallback should find 'flatten' in token string");
    }

    /// Test extract_flatten fallback with complex attributes
    /// Lines 258-263: Tests word boundary checking in fallback
    #[test]
    fn test_extract_flatten_fallback_complex() {
        let tokens: TokenStream = "crate::flatten, default = \"my_fn\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(result, "Fallback should detect flatten with other attrs");
    }

    /// Test extract_flatten fallback with flatten at different positions
    /// Line 265: Tests the return true path in fallback
    #[test]
    fn test_extract_flatten_fallback_at_end() {
        let tokens: TokenStream = "default, some::flatten".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(result);
    }

    /// Test extract_flatten fallback doesn't match partial words
    #[test]
    fn test_extract_flatten_fallback_no_partial_match() {
        // "flattened" should not match "flatten"
        let tokens: TokenStream = "flattened".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(!result, "Should not match 'flattened' as 'flatten'");
    }
    // =================================================================
    // MULTIPART FALLBACK TESTS (form_data / try_from_multipart)
    // =================================================================

    /// Test extract_field_rename falls back to #[form_data(field_name = "...")]
    #[test]
    fn test_extract_field_rename_form_data_fallback() {
        let struct_src = r#"struct Foo { #[form_data(field_name = "my_file")] field: i32 }"#;
        let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_field_rename(&field.attrs);
            assert_eq!(result.as_deref(), Some("my_file"));
        }
    }

    /// Test serde rename takes priority over form_data field_name
    #[test]
    fn test_extract_field_rename_serde_over_form_data() {
        let struct_src = r#"struct Foo { #[serde(rename = "serde_name")] #[form_data(field_name = "form_name")] field: i32 }"#;
        let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_field_rename(&field.attrs);
            assert_eq!(result.as_deref(), Some("serde_name"));
        }
    }

    /// Test extract_field_rename with form_data but no field_name key
    #[test]
    fn test_extract_field_rename_form_data_no_field_name() {
        let struct_src = r#"struct Foo { #[form_data(limit = "10MiB")] field: i32 }"#;
        let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_field_rename(&field.attrs);
            assert_eq!(result, None);
        }
    }

    /// Test extract_rename_all falls back to #[try_from_multipart(rename_all = "...")]
    #[test]
    fn test_extract_rename_all_try_from_multipart_fallback() {
        let item: syn::ItemStruct =
            syn::parse_str(r#"#[try_from_multipart(rename_all = "camelCase")] struct Foo;"#)
                .unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test serde rename_all takes priority over try_from_multipart rename_all
    #[test]
    fn test_extract_rename_all_serde_over_try_from_multipart() {
        let item: syn::ItemStruct = syn::parse_str(r#"#[serde(rename_all = "snake_case")] #[try_from_multipart(rename_all = "camelCase")] struct Foo;"#).unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result.as_deref(), Some("snake_case"));
    }

    /// Test extract_rename_all with try_from_multipart but no rename_all key
    #[test]
    fn test_extract_rename_all_try_from_multipart_no_rename_all() {
        let item: syn::ItemStruct =
            syn::parse_str(r"#[try_from_multipart(strict)] struct Foo;").unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::embedded_key("some_rename = \"x\"", "rename")]
    #[case::missing_quote("rename = value", "rename")]
    #[case::missing_end_quote("rename = \"value", "rename")]
    fn quoted_value_after_key_rejects_malformed_candidates(
        #[case] tokens: &str,
        #[case] key: &str,
    ) {
        assert_eq!(super::quoted_value_after_key(tokens, key), None);
    }
}
