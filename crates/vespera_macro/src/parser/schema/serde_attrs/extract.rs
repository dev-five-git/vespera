use super::fallback::{
    contains_standalone_word, quoted_value_after_key, scan_default_from_raw_tokens,
};

/// Which half of serde a (possibly side-specific) name is read for:
/// `rename(serialize = "a", deserialize = "b")` / `rename_all(serialize = ..,
/// deserialize = ..)` name a member differently per direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerdeSide {
    /// Output (response bodies, and schemas shared by both directions).
    Serialize,
    /// Input only (query / path parameters).
    Deserialize,
}

impl SerdeSide {
    const fn key(self) -> &'static str {
        match self {
            Self::Serialize => "serialize",
            Self::Deserialize => "deserialize",
        }
    }
}

/// The value of a `key = "x"` / `key(serialize = "a", deserialize = "b")`
/// serde key for `side`. A side the list does not name yields `None` (serde
/// then keeps the default name for that side — the other side's value never
/// leaks in).
fn sided_value(
    meta: &syn::meta::ParseNestedMeta<'_>,
    side: SerdeSide,
) -> syn::Result<Option<String>> {
    if meta.input.peek(syn::Token![=]) {
        return Ok(Some(meta.value()?.parse::<syn::LitStr>()?.value()));
    }
    let mut found = None;
    if meta.input.peek(syn::token::Paren) {
        meta.parse_nested_meta(|inner| {
            let value = inner.value()?.parse::<syn::LitStr>()?;
            if inner.path.is_ident(side.key()) {
                found = Some(value.value());
            }
            Ok(())
        })?;
    }
    Ok(found)
}

/// First value of the serde key whose last path segment is `key`, for
/// `side`; the raw-token scan runs only when a `#[serde]` list failed to
/// parse (see [`quoted_value_after_key`], which never reads a side list).
fn extract_sided_key(attrs: &[syn::Attribute], key: &str, side: SerdeSide) -> Option<String> {
    let mut found = None;
    let parsed = walk_serde_keys(attrs, |meta| {
        if meta.path.segments.last().is_none_or(|seg| seg.ident != key) {
            return Ok(false);
        }
        if let Some(value) = sided_value(meta, side)? {
            found.get_or_insert(value);
        }
        Ok(true)
    });
    if found.is_some() || parsed {
        return found;
    }
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .filter_map(|attr| attr.meta.require_list().ok())
        .find_map(|list| quoted_value_after_key(&list.tokens.to_string(), key))
}

/// Container `rename_all` rule (the serialize side).
pub fn extract_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    extract_rename_all_for(attrs, SerdeSide::Serialize)
}

/// Container `rename_all` rule for `side`, falling back to
/// `#[try_from_multipart(rename_all = "...")]`.
pub fn extract_rename_all_for(attrs: &[syn::Attribute], side: SerdeSide) -> Option<String> {
    if let Some(rule) = extract_sided_key(attrs, "rename_all", side) {
        return Some(rule);
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

/// Field / variant `rename` (the serialize side). Schemas are shared by
/// request and response bodies, so they use the serialized name; an input-only
/// `rename(deserialize = "..")` leaves it at the default.
pub fn extract_field_rename(attrs: &[syn::Attribute]) -> Option<String> {
    extract_field_rename_for(attrs, SerdeSide::Serialize)
}

/// Field / variant `rename` for `side`, falling back to
/// `#[form_data(field_name = "...")]`.
pub fn extract_field_rename_for(attrs: &[syn::Attribute], side: SerdeSide) -> Option<String> {
    if let Some(name) = extract_sided_key(attrs, "rename", side) {
        return Some(name);
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

/// Presence flags for the serde skip/other keywords on a field or variant.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerdeSkipFlags {
    pub skip: bool,
    pub skip_serializing: bool,
    pub skip_deserializing: bool,
    pub skip_serializing_if: bool,
    pub other: bool,
}

impl SerdeSkipFlags {
    /// The member never appears on the wire in either direction.
    #[must_use]
    pub const fn fully_skipped(self) -> bool {
        self.skip || (self.skip_serializing && self.skip_deserializing)
    }

    /// The member may be absent from serialized output (`skip_serializing`
    /// or a `skip_serializing_if` predicate), so it must not be `required`.
    #[must_use]
    pub const fn maybe_omitted_on_output(self) -> bool {
        self.skip_serializing || self.skip_serializing_if
    }
}

/// Collect [`SerdeSkipFlags`] from `#[serde(...)]` attributes.
///
/// Unlike the single-keyword extractors, this consumes `key = value` and
/// `key(...)` payloads so an unrelated `skip_serializing_if = "..."` or
/// `rename(serialize = "..")` never aborts the walk; the raw-token fallback
/// still covers anything the structured walk cannot parse.
pub fn extract_serde_skip_flags(attrs: &[syn::Attribute]) -> SerdeSkipFlags {
    let mut flags = SerdeSkipFlags::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let mut local = SerdeSkipFlags::default();
        let parsed = attr.parse_nested_meta(|meta| {
            if let Some(ident) = meta.path.segments.last().map(|seg| &seg.ident) {
                if ident == "skip" {
                    local.skip = true;
                } else if ident == "skip_serializing" {
                    local.skip_serializing = true;
                } else if ident == "skip_deserializing" {
                    local.skip_deserializing = true;
                } else if ident == "skip_serializing_if" {
                    local.skip_serializing_if = true;
                } else if ident == "other" {
                    local.other = true;
                }
            }
            if meta.input.peek(syn::Token![=]) {
                meta.value()?.parse::<syn::Expr>()?;
            } else if meta.input.peek(syn::token::Paren) {
                meta.input.parse::<proc_macro2::Group>()?;
            }
            Ok(())
        });
        if parsed.is_err()
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let tokens = meta_list.tokens.to_string();
            local.skip |= contains_standalone_word(&tokens, "skip");
            local.skip_serializing |= contains_standalone_word(&tokens, "skip_serializing");
            local.skip_deserializing |= contains_standalone_word(&tokens, "skip_deserializing");
            local.skip_serializing_if |= contains_standalone_word(&tokens, "skip_serializing_if");
            local.other |= contains_standalone_word(&tokens, "other");
        }
        flags.skip |= local.skip;
        flags.skip_serializing |= local.skip_serializing;
        flags.skip_deserializing |= local.skip_deserializing;
        flags.skip_serializing_if |= local.skip_serializing_if;
        flags.other |= local.other;
    }
    flags
}

/// Walk every `#[serde(...)]` key, calling `on_key(meta)` for each one and
/// consuming any `= value` / `(...)` payload the callback left unread (it
/// returns `false`), so an unrelated key never aborts the walk.
///
/// Returns `false` when any `#[serde(...)]` list failed to parse.
pub(super) fn walk_serde_keys(
    attrs: &[syn::Attribute],
    mut on_key: impl FnMut(&syn::meta::ParseNestedMeta<'_>) -> syn::Result<bool>,
) -> bool {
    let mut all_parsed = true;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        let parsed = attr.parse_nested_meta(|meta| {
            if !on_key(&meta)? {
                if meta.input.peek(syn::Token![=]) {
                    meta.value()?.parse::<syn::Expr>()?;
                } else if meta.input.peek(syn::token::Paren) {
                    meta.input.parse::<proc_macro2::Group>()?;
                }
            }
            Ok(())
        });
        all_parsed &= parsed.is_ok();
    }
    all_parsed
}

/// `#[serde(rename_all_fields = "...")]` on an enum: the `rename_all` rule
/// applied to the fields of every struct variant.
pub fn extract_rename_all_fields(attrs: &[syn::Attribute]) -> Option<String> {
    let mut found = None;
    walk_serde_keys(attrs, |meta| {
        if meta.path.is_ident("rename_all_fields") && meta.input.peek(syn::Token![=]) {
            let lit: syn::LitStr = meta.value()?.parse()?;
            found.get_or_insert(lit.value());
            return Ok(true);
        }
        Ok(false)
    });
    found
}

/// `#[serde(deny_unknown_fields)]` is present on the container.
pub fn extract_deny_unknown_fields(attrs: &[syn::Attribute]) -> bool {
    let mut found = false;
    walk_serde_keys(attrs, |meta| {
        found |= meta.path.is_ident("deny_unknown_fields");
        Ok(false)
    });
    found
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
    #[case(
        r#"#[serde(rename_all_fields = "camelCase")] enum E {}"#,
        Some("camelCase")
    )]
    #[case(
        r#"#[serde(rename_all = "snake_case", rename_all_fields = "kebab-case")] enum E {}"#,
        Some("kebab-case")
    )]
    #[case(
        r#"#[serde(rename_all_fields = "lowercase")] #[serde(rename_all_fields = "UPPERCASE")] enum E {}"#,
        Some("lowercase")
    )]
    #[case(
        r#"#[serde(rename_all_fields(serialize = "camelCase"))] enum E {}"#,
        None
    )]
    #[case(r"#[serde(rename_all_fields = 1)] enum E {}", None)]
    #[case(r#"#[other(rename_all_fields = "camelCase")] enum E {}"#, None)]
    #[case(r"enum E {}", None)]
    fn rename_all_fields_cases(#[case] src: &str, #[case] expected: Option<&str>) {
        let item: syn::ItemEnum = syn::parse_str(src).unwrap();
        assert_eq!(extract_rename_all_fields(&item.attrs).as_deref(), expected);
    }

    #[rstest]
    #[case(r"#[serde(deny_unknown_fields)] struct S;", true)]
    #[case(
        r#"#[serde(rename_all = "camelCase", deny_unknown_fields)] struct S;"#,
        true
    )]
    #[case(r"#[serde(default)] struct S;", false)]
    #[case(r"struct S;", false)]
    fn deny_unknown_fields_cases(#[case] src: &str, #[case] expected: bool) {
        let item: syn::ItemStruct = syn::parse_str(src).unwrap();
        assert_eq!(extract_deny_unknown_fields(&item.attrs), expected);
    }
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

    /// `rename(serialize = .., deserialize = ..)` names each direction
    /// separately; a side the list does not name keeps the default, and the
    /// raw-token fallback never reads a side list for both directions.
    #[rstest]
    #[case::plain(r#"#[serde(rename = "both")]"#, Some("both"), Some("both"))]
    #[case::both_sides(
        r#"#[serde(rename(serialize = "out", deserialize = "in"))]"#,
        Some("out"),
        Some("in")
    )]
    #[case::deserialize_only(r#"#[serde(rename(deserialize = "in"))]"#, None, Some("in"))]
    #[case::serialize_only(r#"#[serde(rename(serialize = "out"), default)]"#, Some("out"), None)]
    #[case::broken_list_never_mixes(r#"#[serde(@broken, rename(serialize = "out"))]"#, None, None)]
    #[case::non_literal_side(r"#[serde(rename(serialize = 1))]", None, None)]
    fn sided_field_rename(
        #[case] attr_src: &str,
        #[case] serialize: Option<&str>,
        #[case] deserialize: Option<&str>,
    ) {
        let item: syn::ItemStruct = syn::parse_str(&format!("{attr_src} struct Foo;")).unwrap();
        assert_eq!(
            extract_field_rename_for(&item.attrs, SerdeSide::Serialize).as_deref(),
            serialize
        );
        assert_eq!(
            extract_field_rename_for(&item.attrs, SerdeSide::Deserialize).as_deref(),
            deserialize
        );
    }

    #[test]
    fn sided_rename_all() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[serde(rename_all(serialize = "camelCase", deserialize = "kebab-case"))]
        };
        assert_eq!(extract_rename_all(&attrs).as_deref(), Some("camelCase"));
        assert_eq!(
            extract_rename_all_for(&attrs, SerdeSide::Deserialize).as_deref(),
            Some("kebab-case")
        );
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

    #[test]
    fn qualified_default_is_detected() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[serde(module::default)]
        };
        assert_eq!(extract_default(&attrs), Some(None));
    }

    #[rstest]
    #[case(r"#[try_from_multipart(rename_all = 42)] struct Foo;")]
    #[case(r"#[try_from_multipart(strict)] struct Foo;")]
    fn multipart_rename_all_invalid_or_missing(#[case] source: &str) {
        let item: syn::ItemStruct = syn::parse_str(source).unwrap();
        assert_eq!(extract_rename_all(&item.attrs), None);
    }

    #[rstest]
    #[case(r#"#[form_data(limit = "10MiB")] field: i32"#)]
    #[case(r"#[form_data(field_name = 42)] field: i32")]
    fn form_data_field_name_invalid_or_missing(#[case] field_source: &str) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!("struct Foo {{ {field_source} }}")).unwrap();
        let syn::Fields::Named(fields) = &item.fields else {
            unreachable!("test input must contain named fields");
        };
        assert_eq!(extract_field_rename(&fields.named[0].attrs), None);
    }

    #[rstest]
    #[case::none("#[doc = \"x\"]", SerdeSkipFlags::default())]
    #[case::skip("#[serde(skip)]", SerdeSkipFlags { skip: true, ..SerdeSkipFlags::default() })]
    #[case::skip_serializing_if(
        "#[serde(skip_serializing_if = \"Vec::is_empty\", default)]",
        SerdeSkipFlags { skip_serializing_if: true, ..SerdeSkipFlags::default() }
    )]
    #[case::skip_ser_only(
        "#[serde(rename(serialize = \"a\"), skip_serializing)]",
        SerdeSkipFlags { skip_serializing: true, ..SerdeSkipFlags::default() }
    )]
    #[case::both_directions(
        "#[serde(skip_serializing)] #[serde(skip_deserializing)]",
        SerdeSkipFlags {
            skip_serializing: true,
            skip_deserializing: true,
            ..SerdeSkipFlags::default()
        }
    )]
    #[case::other("#[serde(other)]", SerdeSkipFlags { other: true, ..SerdeSkipFlags::default() })]
    fn serde_skip_flags_cases(#[case] attrs_src: &str, #[case] expected: SerdeSkipFlags) {
        let item: syn::ItemStruct =
            syn::parse_str(&format!("struct Foo {{ {attrs_src} field: i32 }}")).unwrap();
        let syn::Fields::Named(fields) = &item.fields else {
            unreachable!("named fields");
        };
        assert_eq!(extract_serde_skip_flags(&fields.named[0].attrs), expected);
    }

    #[test]
    fn serde_skip_flags_helpers() {
        let only_ser = SerdeSkipFlags {
            skip_serializing: true,
            ..SerdeSkipFlags::default()
        };
        assert!(only_ser.maybe_omitted_on_output());
        assert!(!only_ser.fully_skipped());
        let both = SerdeSkipFlags {
            skip_deserializing: true,
            ..only_ser
        };
        assert!(both.fully_skipped());
        assert!(!SerdeSkipFlags::default().maybe_omitted_on_output());
    }

    #[test]
    fn serde_skip_flags_fallback_after_parse_error() {
        // `skip_serializing_if` followed by an unparsable payload aborts the
        // structured walk; the raw-token fallback still finds the keyword.
        let attr: syn::Attribute = syn::parse_quote!(#[serde(skip_serializing_if = , other)]);
        let flags = extract_serde_skip_flags(&[attr]);
        assert!(flags.skip_serializing_if);
        assert!(flags.other);
    }
}
