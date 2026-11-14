use vespera_core::schema::{Schema, SchemaType};

use super::super::{
    request_view::current_side,
    serde_attrs::{SerdeSide, extract_field_rename_for, rename_variant, strip_raw_prefix_owned},
};

/// Parse a simple enum (all unit variants) to a string schema with enum values.
pub(super) fn parse_unit_enum_to_schema(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    rename_all: Option<&str>,
) -> Schema {
    let mut enum_values = Vec::with_capacity(enum_item.variants.len());

    for variant in enum_item
        .variants
        .iter()
        .filter(|variant| !super::variant::is_variant_omitted(variant))
    {
        enum_values.push(serde_json::Value::String(get_variant_key(
            variant, rename_all,
        )));
    }

    Schema {
        schema_type: Some(SchemaType::String),
        description,
        r#enum: if enum_values.is_empty() {
            None
        } else {
            Some(enum_values)
        },
        ..Schema::string()
    }
}

/// Get the variant key (name after rename transformations) on the serde side
/// of the view being built.
pub(super) fn get_variant_key(variant: &syn::Variant, rename_all: Option<&str>) -> String {
    let variant_name = strip_raw_prefix_owned(variant.ident.to_string());

    extract_field_rename_for(&variant.attrs, current_side())
        .unwrap_or_else(|| rename_variant(&variant_name, rename_all))
}

/// Whether the enum derives `serde_repr`'s impl for `side`:
/// `Serialize_repr` (serialize) / `Deserialize_repr` (deserialize), by any
/// path (`serde_repr::Serialize_repr`).
///
/// `#[derive(Schema)]` cannot see derives listed in its own `#[derive(..)]`
/// attribute, so it copies them from the source file into the stored
/// definition (see `schema_impl::serde_repr_derives`).
pub fn derives_serde_repr(attrs: &[syn::Attribute], side: SerdeSide) -> bool {
    let wanted = match side {
        SerdeSide::Serialize => "Serialize_repr",
        SerdeSide::Deserialize => "Deserialize_repr",
    };
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .any(|attr| {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                found |= meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|seg| seg.ident == wanted);
                Ok(())
            });
            found
        })
}

/// A `serde_repr` enum on the current side is its integer discriminant:
/// `{type: integer, enum: [..]}`. Explicit discriminants are taken literally
/// (`= 3`, `= -1`), implicit ones count up from the previous variant (`0`
/// for the first). serde attributes (`rename`, `skip`) do not apply. A
/// discriminant that is not an integer literal (`= BASE + 1`) cannot be
/// evaluated here, so the schema is a plain `{type: integer}`.
pub(super) fn parse_serde_repr_enum(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
) -> Option<Schema> {
    if !derives_serde_repr(&enum_item.attrs, current_side())
        || !enum_item
            .variants
            .iter()
            .all(|variant| matches!(variant.fields, syn::Fields::Unit))
    {
        return None;
    }
    let mut next = Some(0_i128);
    let mut values = Some(Vec::with_capacity(enum_item.variants.len()));
    for variant in &enum_item.variants {
        let value = variant
            .discriminant
            .as_ref()
            .map_or(next, |(_, expr)| integer_literal(expr));
        next = value.and_then(|value| value.checked_add(1));
        match (value.and_then(json_integer), values.as_mut()) {
            (Some(json), Some(list)) => list.push(json),
            _ => values = None,
        }
    }
    Some(Schema {
        description,
        r#enum: values.filter(|values| !values.is_empty()),
        ..Schema::integer()
    })
}

/// `value` as a JSON number (`None` outside the `i64` / `u64` range).
fn json_integer(value: i128) -> Option<serde_json::Value> {
    i64::try_from(value)
        .map(serde_json::Value::from)
        .ok()
        .or_else(|| u64::try_from(value).ok().map(serde_json::Value::from))
}

/// The value of an integer literal discriminant, `-` prefix included.
fn integer_literal(expr: &syn::Expr) -> Option<i128> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(int),
            ..
        }) => int.base10_parse().ok(),
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => integer_literal(expr).map(|value| -value),
        syn::Expr::Paren(paren) => integer_literal(&paren.expr),
        syn::Expr::Group(group) => integer_literal(&group.expr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use rstest::rstest;

    use super::super::{super::request_view::with_request_view, parse_enum_to_schema};

    fn schema_json(src: &str, request: bool) -> String {
        let item: syn::ItemEnum = syn::parse_str(src).unwrap();
        let build = || {
            parse_enum_to_schema(
                &item,
                &HashSet::<String>::new(),
                &HashMap::<String, String>::new(),
            )
        };
        let schema = if request {
            with_request_view(build)
        } else {
            build()
        };
        serde_json::to_string(&schema).unwrap()
    }

    /// `serde_repr` enums are their integer discriminant on the side that
    /// derives it; the other side keeps serde's variant names.
    #[rstest]
    #[case::implicit(
        "serde_repr_implicit",
        "#[derive(Serialize_repr)] #[repr(u8)] enum E { A, B, C }",
        false
    )]
    #[case::explicit_and_increment(
        "serde_repr_explicit",
        "#[derive(serde_repr::Serialize_repr)] #[repr(i16)] enum E { A = -2, B, C = 10, D }",
        false
    )]
    #[case::ignores_serde_renames(
        "serde_repr_ignores_renames",
        r#"#[derive(Serialize_repr)] #[serde(rename_all = "lowercase")] enum E { #[serde(rename = "x")] A = 1, B }"#,
        false
    )]
    #[case::non_literal_discriminant(
        "serde_repr_non_literal",
        "#[derive(Serialize_repr)] #[repr(u8)] enum E { A = BASE, B }",
        false
    )]
    #[case::request_side_without_deserialize_repr(
        "serde_repr_request_names",
        "#[derive(Serialize_repr)] #[repr(u8)] enum E { A, B }",
        true
    )]
    #[case::request_side_deserialize_repr(
        "serde_repr_request_integer",
        "#[derive(Deserialize_repr)] #[repr(u8)] enum E { A = 3, B }",
        true
    )]
    #[case::response_side_deserialize_repr_only(
        "serde_repr_response_names",
        "#[derive(Deserialize_repr)] #[repr(u8)] enum E { A = 3, B }",
        false
    )]
    fn serde_repr_enums(#[case] snapshot: &str, #[case] src: &str, #[case] request: bool) {
        insta::assert_snapshot!(snapshot, schema_json(src, request));
    }

    /// Parenthesised and invisible-group (`macro_rules!` `$e:expr`)
    /// discriminants are evaluated through to their literal.
    #[rstest]
    #[case::paren(syn::parse_quote!((-4)), Some(-4))]
    #[case::group(
        syn::Expr::Group(syn::ExprGroup {
            attrs: Vec::new(),
            group_token: syn::token::Group::default(),
            expr: Box::new(syn::parse_quote!(7)),
        }),
        Some(7)
    )]
    #[case::path(syn::parse_quote!(BASE), None)]
    fn integer_literal_unwraps_parens_and_groups(
        #[case] expr: syn::Expr,
        #[case] expected: Option<i128>,
    ) {
        assert_eq!(super::integer_literal(&expr), expected);
    }

    /// The request (`{Name}Input`) view of an enum follows the deserialize
    /// side: sided renames, `skip_deserializing` vs `skip_serializing`
    /// variants, and struct-variant field defaults.
    #[rstest]
    #[case::sided_rename_all(
        "enum_view_sided_rename_all",
        r#"#[serde(rename_all(serialize = "UPPERCASE", deserialize = "lowercase"))] enum E { Alpha, Beta }"#
    )]
    #[case::sided_variant_rename(
        "enum_view_sided_variant_rename",
        r#"enum E { #[serde(rename(serialize = "out", deserialize = "in"))] A, B }"#
    )]
    #[case::one_sided_skips(
        "enum_view_one_sided_skips",
        "enum E { A, #[serde(skip_serializing)] ReadOnly, #[serde(skip_deserializing)] WriteOnly }"
    )]
    #[case::variant_field_default(
        "enum_view_variant_field_default",
        "enum E { A { #[serde(default)] x: u8, y: u8 } }"
    )]
    #[case::sided_rename_all_fields(
        "enum_view_sided_rename_all_fields",
        r#"#[serde(tag = "t", rename_all_fields(serialize = "camelCase", deserialize = "SCREAMING_SNAKE_CASE"))] enum E { A { field_one: u8 } }"#
    )]
    fn enum_views_follow_serde_side(#[case] snapshot: &str, #[case] src: &str) {
        insta::assert_snapshot!(
            snapshot,
            format!(
                "response: {}\nrequest:  {}",
                schema_json(src, false),
                schema_json(src, true)
            )
        );
    }
}
