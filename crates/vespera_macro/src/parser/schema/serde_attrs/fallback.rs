pub(super) fn quoted_value_after_key(tokens: &str, key: &str) -> Option<String> {
    for start in key_positions(tokens, key) {
        // Only the `key = "value"` form: a side list `key(serialize = "a",
        // deserialize = "b")` must never yield one side's value for both.
        let Some(value_part) = tokens[start + key.len()..].trim_start().strip_prefix('=') else {
            continue;
        };
        if let Some(value) = leading_string_literal(value_part) {
            return Some(value);
        }
    }
    None
}

/// The value of the `"..."` literal `value_part` starts with (after
/// whitespace); a value of any other kind yields `None`. The closing quote is
/// the first one not escaped by a backslash, and the literal's escapes
/// (`\"`, `\\`, `\u{..}`) are decoded by `syn`, as rustc decodes them.
fn leading_string_literal(value_part: &str) -> Option<String> {
    let body = value_part.trim_start().strip_prefix('"')?;
    let mut escaped = false;
    let end = body.char_indices().find_map(|(index, ch)| {
        if escaped {
            escaped = false;
            None
        } else if ch == '\\' {
            escaped = true;
            None
        } else {
            (ch == '"').then_some(index)
        }
    })?;
    syn::parse_str::<syn::LitStr>(&format!("\"{}\"", &body[..end]))
        .ok()
        .map(|literal| literal.value())
}

pub(super) fn contains_standalone_word(tokens: &str, word: &str) -> bool {
    key_positions(tokens, word).next().is_some()
}

/// Start indices of `word` in `tokens` written as a key: a standalone word
/// or the last segment of a qualified path (`serde::default`), never inside
/// a string literal (`skip_serializing_if = "util::skip_none"` names no key).
fn key_positions<'a>(tokens: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    tokens
        .match_indices(word)
        .map(|(start, _)| start)
        .filter(move |&start| {
            (is_standalone_word_at(tokens, start, word) || is_qualified_key(tokens, start, word))
                && !in_string_literal(tokens, start)
        })
}

/// Whether byte `at` of `tokens` lies inside a `"..."` literal (escapes
/// honoured).
fn in_string_literal(tokens: &str, at: usize) -> bool {
    let mut in_str = false;
    let mut escaped = false;
    for ch in tokens[..at].chars() {
        if escaped {
            escaped = false;
        } else if in_str && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            in_str = !in_str;
        }
    }
    in_str
}

/// `word` at `start` is the last segment of a qualified path: `::` before it
/// and a key boundary after it (`a::skip_none` is not `skip`).
fn is_qualified_key(tokens: &str, start: usize, word: &str) -> bool {
    tokens[..start].ends_with("::") && is_key_end(tokens, start + word.len())
}

/// The character at byte `end` ends a key (or the input ends there).
fn is_key_end(tokens: &str, end: usize) -> bool {
    matches!(
        tokens[end..].chars().next(),
        None | Some(' ' | ',' | ')' | '=')
    )
}

fn is_standalone_word_at(tokens: &str, start: usize, word: &str) -> bool {
    matches!(tokens[..start].chars().last(), None | Some(' ' | ',' | '('))
        && is_key_end(tokens, start + word.len())
}

/// `default` / `default = "path"` among raw `#[serde(...)]` tokens, skipping
/// any occurrence that is no key (inside a string literal such as
/// `rename = "defaultValue"`, or part of a longer identifier) and a
/// `default = <non-string>` (not a serde default).
#[allow(clippy::option_option)]
pub(super) fn scan_default_from_raw_tokens(tokens: &str) -> Option<Option<String>> {
    for start in key_positions(tokens, "default") {
        let remaining = tokens[start + "default".len()..].trim_start();
        let Some(after_equals) = remaining.strip_prefix('=') else {
            return Some(None);
        };
        if let Some(path) = leading_string_literal(after_equals) {
            return Some(Some(path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::option_option)]

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

    /// `rename_all` through the structured walk.
    #[test]
    fn test_extract_rename_all_structured_path() {
        // Standard path - parse_nested_meta should work
        let attrs = get_struct_attrs(r#"rename_all = "camelCase""#);
        let result = extract_rename_all(&attrs);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// `rename` through the structured walk.
    #[test]
    fn test_extract_field_rename_structured_path() {
        // Standard path
        let attrs = get_field_attrs(r#"rename = "myField""#);
        let result = extract_field_rename(&attrs);
        assert_eq!(result.as_deref(), Some("myField"));
    }

    /// Bare `default` through the structured walk.
    #[test]
    fn test_extract_default_standalone_structured_path() {
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

    /// Test that a "default" occurring as part of a longer identifier is not
    /// a serde default: `scan_default_from_raw_tokens` skips it (the
    /// `key_positions` candidates must be whole keys), so the scan finds
    /// nothing and returns `None`.
    #[test]
    fn test_extract_default_substring_in_identifier_is_not_a_match() {
        // `field_default` contains "default" only as a suffix of an
        // identifier: the character before it is `_`, which is not a key
        // boundary, so it is not a key candidate.
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
        let syn::Fields::Named(fields) = &item.fields else {
            unreachable!("test input must contain named fields");
        };
        let attrs = &fields.named.first().unwrap().attrs;
        let rename = extract_field_rename(attrs);
        let default = extract_default(attrs);
        assert_eq!(rename.as_deref(), Some("customName"));
        assert_eq!(default, Some(None));
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

    /// Raw-token fallback (the leading `@broken` fails the structured walk).
    #[test]
    fn test_extract_rename_all_fallback_manual_parsing() {
        let tokens = quote!(@broken, rename_all = "kebab-case");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("kebab-case"));
    }

    /// Raw-token fallback among other keys.
    #[test]
    fn test_extract_rename_all_complex_attribute_fallback() {
        let tokens = quote!(@broken, default, rename_all = "SCREAMING_SNAKE_CASE", skip);
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

    /// Raw-token fallback reading a closed literal (an unclosed one cannot
    /// be lexed into a token stream; `quoted_value_after_key` rejects it, see
    /// `quoted_value_after_key_rejects_malformed_candidates`).
    #[test]
    fn test_extract_rename_all_fallback_string_literal() {
        let tokens = quote!(@broken, rename_all = "camelCase");
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

    // Qualified keys in the raw-token fallback: every token stream below
    // starts with `@broken`, which fails the structured walk.

    /// Qualified `rename_all` key in the fallback.
    #[test]
    fn test_extract_rename_all_qualified_path_forces_fallback() {
        let tokens = quote!(@broken, serde_with::rename_all = "camelCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    /// Test extract_rename_all with another qualified path variation
    #[test]
    fn test_extract_rename_all_module_qualified_forces_fallback() {
        let tokens = quote!(@broken, my_module::rename_all = "snake_case");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("snake_case"));
    }

    /// Test extract_rename_all with deeply qualified path
    #[test]
    fn test_extract_rename_all_deeply_qualified_forces_fallback() {
        let tokens = quote!(@broken, a::b::rename_all = "PascalCase");
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("PascalCase"));
    }

    /// A qualified key whose prefix is a longer identifier is still matched by
    /// its last segment.
    #[test]
    fn test_extract_rename_all_raw_tokens_force_fallback() {
        let tokens: TokenStream = "@broken, __rename_all_prefix::rename_all = \"lowercase\""
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
        let tokens: TokenStream = "@broken, crate::rename_all = \"UPPERCASE\""
            .parse()
            .unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("UPPERCASE"));
    }

    /// Test with self:: prefix
    #[test]
    fn test_extract_rename_all_self_qualified_forces_fallback() {
        let tokens: TokenStream = "@broken, self::rename_all = \"kebab-case\""
            .parse()
            .unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_rename_all(&[attr]);
        assert_eq!(result.as_deref(), Some("kebab-case"));
    }

    // `rename` / `flatten` in the raw-token fallback (again behind a
    // leading `@broken`).

    /// Qualified `rename` key in the fallback.
    #[test]
    fn test_extract_field_rename_fallback_word_boundary() {
        let tokens: TokenStream = "@broken, my_module::rename = \"value\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        assert_eq!(result.as_deref(), Some("value"));
    }

    /// Qualified `rename` key among other keys in the fallback.
    #[test]
    fn test_extract_field_rename_fallback_complex_attr() {
        let tokens: TokenStream = "@broken, crate::other::rename = \"custom_field\", default"
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
        let tokens: TokenStream = "@broken, some::rename_all = \"camelCase\"".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_field_rename(&[attr]);
        // Should NOT match rename_all as rename
        assert_eq!(result, None);
    }

    /// Qualified `flatten` key in the fallback.
    #[test]
    fn test_extract_flatten_fallback_path() {
        let tokens: TokenStream = "@broken, my_module::flatten".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(result, "Fallback should find 'flatten' in token string");
    }

    /// Qualified `flatten` key before other keys in the fallback.
    #[test]
    fn test_extract_flatten_fallback_complex() {
        let tokens: TokenStream = "@broken, crate::flatten, default = \"my_fn\""
            .parse()
            .unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(result, "Fallback should detect flatten with other attrs");
    }

    /// Qualified `flatten` key after other keys in the fallback.
    #[test]
    fn test_extract_flatten_fallback_at_end() {
        let tokens: TokenStream = "@broken, default, some::flatten".parse().unwrap();
        let attr = create_attr_with_raw_tokens(tokens);
        let result = extract_flatten(&[attr]);
        assert!(result);
    }

    /// Test extract_flatten fallback doesn't match partial words
    #[test]
    fn test_extract_flatten_fallback_no_partial_match() {
        // "flattened" should not match "flatten"
        let tokens: TokenStream = "@broken, flattened".parse().unwrap();
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
        let syn::Fields::Named(fields) = &item.fields else {
            unreachable!("test input must contain named fields");
        };
        let field = fields.named.first().unwrap();
        let result = extract_field_rename(&field.attrs);
        assert_eq!(result.as_deref(), Some("my_file"));
    }

    /// Test serde rename takes priority over form_data field_name
    #[test]
    fn test_extract_field_rename_serde_over_form_data() {
        let struct_src = r#"struct Foo { #[serde(rename = "serde_name")] #[form_data(field_name = "form_name")] field: i32 }"#;
        let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let syn::Fields::Named(fields) = &item.fields else {
            unreachable!("test input must contain named fields");
        };
        let field = fields.named.first().unwrap();
        let result = extract_field_rename(&field.attrs);
        assert_eq!(result.as_deref(), Some("serde_name"));
    }

    /// Test extract_field_rename with form_data but no field_name key
    #[test]
    fn test_extract_field_rename_form_data_no_field_name() {
        let struct_src = r#"struct Foo { #[form_data(limit = "10MiB")] field: i32 }"#;
        let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
        let syn::Fields::Named(fields) = &item.fields else {
            unreachable!("test input must contain named fields");
        };
        let field = fields.named.first().unwrap();
        let result = extract_field_rename(&field.attrs);
        assert_eq!(result, None);
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
    #[case::longer_key("rename_all = \"camelCase\"", "rename")]
    #[case::qualified_longer_key("serde::rename_all = \"camelCase\"", "rename")]
    fn quoted_value_after_key_rejects_malformed_candidates(
        #[case] tokens: &str,
        #[case] key: &str,
    ) {
        assert_eq!(super::quoted_value_after_key(tokens, key), None);
    }

    /// A qualified key with no spaces around `::`, read at the string level
    /// (the `quote!` / proc-macro2 fallback printers space `::` out, so an
    /// attribute-level test never reaches this form): only the `::` directly
    /// before the key makes it a key (`is_qualified_key`), not a standalone
    /// word.
    #[rstest]
    #[case::qualified_key(
        "serde_with::rename_all = \"camelCase\"",
        "rename_all",
        Some("camelCase")
    )]
    #[case::deep_qualified_key("a::b::rename = \"x\"", "rename", Some("x"))]
    #[case::qualified_key_after_others(
        "skip, serde_with::rename_all = \"camelCase\"",
        "rename_all",
        Some("camelCase")
    )]
    fn quoted_value_after_qualified_key(
        #[case] tokens: &str,
        #[case] key: &str,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            super::quoted_value_after_key(tokens, key).as_deref(),
            expected
        );
    }

    /// The value is the literal's value (escapes decoded like rustc reads
    /// it), not the text up to its first `"` byte.
    #[rstest]
    #[case::escaped_quote(r#"rename = "a\"b""#, "rename", Some("a\"b"))]
    #[case::escaped_backslash(r#"rename = "a\\""#, "rename", Some("a\\"))]
    #[case::unicode_escape(r#"rename = "\u{e9}t\u{e9}""#, "rename", Some("été"))]
    #[case::next_key_after_escape(r#"rename = "a\"b", default"#, "rename", Some("a\"b"))]
    fn quoted_value_decodes_escapes(
        #[case] tokens: &str,
        #[case] key: &str,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            super::quoted_value_after_key(tokens, key).as_deref(),
            expected
        );
    }

    #[rstest]
    #[case("default", "default", true)]
    #[case("(default)", "default", true)]
    #[case("default,", "default", true)]
    #[case("xdefault", "default", false)]
    #[case("defaultx", "default", false)]
    fn standalone_word_boundaries(
        #[case] tokens: &str,
        #[case] word: &str,
        #[case] expected: bool,
    ) {
        let start = tokens.find(word).unwrap();
        assert_eq!(super::is_standalone_word_at(tokens, start, word), expected);
    }

    #[rstest]
    #[case("module::rename", 8, "rename", true)]
    #[case("module:rename", 7, "rename", false)]
    #[case("rename", 0, "rename", false)]
    #[case("a::skip_none", 3, "skip", false)]
    #[case("a::skip)", 3, "skip", true)]
    fn qualified_key_detection(
        #[case] tokens: &str,
        #[case] start: usize,
        #[case] word: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(super::is_qualified_key(tokens, start, word), expected);
    }

    /// Raw-token keys never match inside a string literal (escapes honoured)
    /// nor as the prefix of a longer qualified identifier.
    #[rstest]
    #[case::literal_path(r#"skip_serializing_if = "util::skip_none""#, "skip", false)]
    #[case::literal_word(r#"with = "a skip b""#, "skip", false)]
    #[case::escaped_quote_stays_in_literal(r#"rename = "a\" skip""#, "skip", false)]
    #[case::escaped_backslash_closes_literal(r#"rename = "a\\", skip"#, "skip", true)]
    #[case::qualified_key(r#"with = "x", serde::flatten"#, "flatten", true)]
    #[case::literal_flatten(r#"with = "helpers::flatten_opt""#, "flatten", false)]
    fn standalone_word_ignores_literals(
        #[case] tokens: &str,
        #[case] word: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(super::contains_standalone_word(tokens, word), expected);
    }

    #[rstest]
    #[case::after_literal_mention(r#"rename = "defaultValue", default"#, Some(None))]
    #[case::non_string_then_path(r#"default = 1, default = "f""#, Some(Some("f")))]
    #[case::non_string_only("default = 1", None)]
    #[case::qualified_path(r#"serde::default = "f""#, Some(Some("f")))]
    #[case::longer_ident("defaults", None)]
    #[case::literal_only(r#"rename = "default""#, None)]
    fn raw_default_scan(#[case] tokens: &str, #[case] expected: Option<Option<&str>>) {
        assert_eq!(
            super::scan_default_from_raw_tokens(tokens),
            expected.map(|inner| inner.map(str::to_string))
        );
    }

    #[rstest]
    #[case::default_after_rename(r#"#[serde(rename = "defaultValue", default)]"#, Some(None))]
    #[case::path_after_rename(r#"#[serde(rename = "x", default = "f")]"#, Some(Some("f")))]
    #[case::non_string_value("#[serde(default = 1)]", None)]
    #[case::first_attribute_wins(r#"#[serde(default)] #[serde(default = "f")]"#, Some(None))]
    #[case::qualified("#[serde(serde::default)]", Some(None))]
    #[case::absent(r#"#[serde(rename = "default")]"#, None)]
    fn default_detection_skips_earlier_keys(
        #[case] attrs: &str,
        #[case] expected: Option<Option<&str>>,
    ) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!("struct S {{ {attrs} f: i32 }}")).unwrap();
        let attrs = &item.fields.iter().next().unwrap().attrs;
        assert_eq!(
            extract_default(attrs),
            expected.map(|inner| inner.map(str::to_string))
        );
    }

    #[test]
    fn default_raw_fallback_skips_literal_mentions() {
        let attr = create_attr_with_raw_tokens(quote!(@, rename = "defaultValue", default));
        assert_eq!(extract_default(&[attr]), Some(None));
    }

    #[rstest]
    #[case::predicate_path(r#"#[serde(skip_serializing_if = "util::skip_none")]"#, false)]
    #[case::predicate_then_skip(r#"#[serde(skip_serializing_if = "Option::is_none", skip)]"#, true)]
    #[case::split_lists("#[serde(skip_serializing)] #[serde(skip_deserializing)]", true)]
    #[case::one_side("#[serde(skip_serializing)]", false)]
    fn skip_detection(#[case] attrs: &str, #[case] expected: bool) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!("struct S {{ {attrs} f: i32 }}")).unwrap();
        assert_eq!(
            extract_skip(&item.fields.iter().next().unwrap().attrs),
            expected
        );
    }

    #[rstest]
    #[case::with_path(r#"#[serde(with = "helpers::flatten_opt")]"#, false)]
    #[case::with_then_flatten(r#"#[serde(with = "x", flatten)]"#, true)]
    #[case::qualified("#[serde(serde::flatten)]", true)]
    #[case::second_list(r#"#[serde(rename = "a")] #[serde(flatten)]"#, true)]
    fn flatten_detection(#[case] attrs: &str, #[case] expected: bool) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!("struct S {{ {attrs} f: i32 }}")).unwrap();
        assert_eq!(
            extract_flatten(&item.fields.iter().next().unwrap().attrs),
            expected
        );
    }

    #[rstest]
    #[case::raw_flatten(quote!(@, flatten), true)]
    #[case::raw_literal_only(quote!(@, with = "a::flatten"), false)]
    fn flatten_raw_fallback(#[case] tokens: TokenStream, #[case] expected: bool) {
        assert_eq!(
            extract_flatten(&[create_attr_with_raw_tokens(tokens)]),
            expected
        );
    }
}
