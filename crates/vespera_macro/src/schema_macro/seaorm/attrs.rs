/// One `key` / `key = value` entry of a `#[sea_orm(...)]` attribute.
struct SeaOrmEntry {
    key: String,
    value: Option<syn::Expr>,
}

/// Every entry of the field's `#[sea_orm(...)]` attributes, parsed as
/// proper meta items: values may be any expression (`"x"`, `false`, `-1`,
/// `0.5`, a string containing commas), list entries (`key(..)`) are skipped.
/// A malformed attribute contributes the entries parsed before the error.
fn sea_orm_entries(attrs: &[syn::Attribute]) -> Vec<SeaOrmEntry> {
    let mut entries = Vec::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("sea_orm")) {
        let _ = attr.parse_nested_meta(|meta| {
            let key = meta
                .path
                .get_ident()
                .map_or_else(String::new, ToString::to_string);
            let value = if meta.input.peek(syn::Token![=]) {
                Some(meta.value()?.parse::<syn::Expr>()?)
            } else {
                if meta.input.peek(syn::token::Paren) {
                    let content;
                    syn::parenthesized!(content in meta.input);
                    content.parse::<proc_macro2::TokenStream>()?;
                }
                None
            };
            entries.push(SeaOrmEntry { key, value });
            Ok(())
        });
    }
    entries
}

fn entry<'a>(entries: &'a [SeaOrmEntry], key: &str) -> Option<&'a SeaOrmEntry> {
    entries.iter().find(|entry| entry.key == key)
}

fn string_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(lit),
            ..
        }) => Some(lit.value()),
        _ => None,
    }
}

/// Extract a named string value from a `sea_orm` attribute.
fn extract_sea_orm_attr_value(attrs: &[syn::Attribute], attr_name: &str) -> Option<String> {
    entry(&sea_orm_entries(attrs), attr_name)?
        .value
        .as_ref()
        .and_then(string_value)
}

/// Extract the `from` field name from a `sea_orm` relation attribute.
///
/// SeaORM accepts a field name (`user_id`), a column (`UserId`,
/// `Column::UserId`) and a composite list (`(a, b)`); the column forms are
/// normalized to the field name (`user_id`), a composite list to
/// `(a, b)` of field names (see [`is_composite_columns`]).
pub fn extract_belongs_to_from_field(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "from").map(|value| normalize_columns(&value))
}

/// Extract the `to` field name (the referenced column) from a `sea_orm`
/// relation attribute, normalized like [`extract_belongs_to_from_field`].
pub fn extract_belongs_to_to_field(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "to").map(|value| normalize_columns(&value))
}

/// Whether a normalized `from` / `to` value lists several columns.
pub fn is_composite_columns(columns: &str) -> bool {
    columns.starts_with('(')
}

/// `Column::UserId` / `UserId` / `user_id` -> `user_id`; `(A, b)` ->
/// `(a, b)`. A value that is no column path is kept as written.
fn normalize_columns(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(list) = trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let columns: Vec<String> = list
            .split(',')
            .map(str::trim)
            .filter(|column| !column.is_empty())
            .map(column_field_name)
            .collect();
        return format!("({})", columns.join(", "));
    }
    column_field_name(trimmed)
}

/// The model field behind one column (`Column::UserId` -> `user_id`).
fn column_field_name(column: &str) -> String {
    let last = column.rsplit("::").next().unwrap_or(column).trim();
    let last = last.strip_prefix("r#").unwrap_or(last);
    if !last.starts_with(|c: char| c.is_ascii_uppercase()) {
        return last.to_string();
    }
    let mut field = String::with_capacity(last.len() + 4);
    for (index, c) in last.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if index > 0 {
                field.push('_');
            }
            field.push(c.to_ascii_lowercase());
        } else {
            field.push(c);
        }
    }
    field
}

/// Extract the `relation_enum` value from a `sea_orm` attribute.
pub fn extract_relation_enum(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "relation_enum")
}

/// Extract the `via_rel` value from a `sea_orm` attribute.
pub fn extract_via_rel(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "via_rel")
}

/// Extract `default_value` from a `sea_orm` attribute: a string literal's
/// contents, any other literal (number, bool, negative number) as written.
pub fn extract_sea_orm_default_value(attrs: &[syn::Attribute]) -> Option<String> {
    let entries = sea_orm_entries(attrs);
    let expr = entry(&entries, "default_value")?.value.as_ref()?;
    string_value(expr).or_else(|| {
        let tokens = quote::ToTokens::to_token_stream(expr).to_string();
        Some(tokens.replace(' ', ""))
    })
}

/// Check if a `sea_orm(default_value)` is a SQL function.
pub fn is_sql_function_default(value: &str) -> bool {
    value.contains('(')
}

/// Check if a field is a database-generated primary key:
/// `#[sea_orm(primary_key)]` without `auto_increment = false` (SeaORM's
/// default for a primary key is auto-increment).
pub fn has_sea_orm_primary_key(attrs: &[syn::Attribute]) -> bool {
    let entries = sea_orm_entries(attrs);
    let auto_increment = entry(&entries, "auto_increment")
        .and_then(|e| e.value.as_ref())
        .is_none_or(is_true);
    is_primary_key_entry(&entries) && auto_increment
}

/// Whether a field is (part of) the primary key, `auto_increment` or not.
pub fn is_sea_orm_primary_key(attrs: &[syn::Attribute]) -> bool {
    is_primary_key_entry(&sea_orm_entries(attrs))
}

fn is_primary_key_entry(entries: &[SeaOrmEntry]) -> bool {
    entry(entries, "primary_key").is_some_and(|pk| pk.value.as_ref().is_none_or(is_true))
}

fn is_true(expr: &syn::Expr) -> bool {
    !matches!(
        expr,
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(syn::LitBool { value: false, .. }),
            ..
        })
    )
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn test_extract_belongs_to_from_field_with_from() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id", to = "id")])];
        assert_eq!(
            extract_belongs_to_from_field(&attrs),
            Some("user_id".to_string())
        );
    }

    #[test]
    fn test_extract_belongs_to_from_field_without_from() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(belongs_to, to = "id")])];
        assert_eq!(extract_belongs_to_from_field(&attrs), None);
    }

    #[test]
    fn test_extract_belongs_to_from_field_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        assert_eq!(extract_belongs_to_from_field(&attrs), None);
    }

    #[test]
    fn test_extract_belongs_to_from_field_empty_attrs() {
        assert_eq!(extract_belongs_to_from_field(&[]), None);
    }

    #[test]
    fn test_extract_relation_enum_with_value() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[sea_orm(belongs_to, relation_enum = "TargetUser", from = "target_user_id", to = "id")]),
        ];
        assert_eq!(
            extract_relation_enum(&attrs),
            Some("TargetUser".to_string())
        );
    }

    #[test]
    fn test_extract_relation_enum_without_relation_enum() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id", to = "id")])];
        assert_eq!(extract_relation_enum(&attrs), None);
    }

    #[test]
    fn test_extract_relation_enum_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        assert_eq!(extract_relation_enum(&attrs), None);
    }

    #[test]
    fn test_extract_relation_enum_empty_attrs() {
        assert_eq!(extract_relation_enum(&[]), None);
    }

    #[test]
    fn test_extract_via_rel_with_value() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(has_many, via_rel = "TargetUser")])];
        assert_eq!(extract_via_rel(&attrs), Some("TargetUser".to_string()));
    }

    #[test]
    fn test_extract_via_rel_with_relation_enum() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[sea_orm(has_many, relation_enum = "TargetUserNotifications", via_rel = "TargetUser")]),
        ];
        assert_eq!(extract_via_rel(&attrs), Some("TargetUser".to_string()));
    }

    #[test]
    fn test_extract_via_rel_without_via_rel() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(has_many, relation_enum = "Memos")])];
        assert_eq!(extract_via_rel(&attrs), None);
    }

    #[test]
    fn test_extract_via_rel_non_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        assert_eq!(extract_via_rel(&attrs), None);
    }

    #[test]
    fn test_extract_via_rel_empty_attrs() {
        assert_eq!(extract_via_rel(&[]), None);
    }

    #[test]
    fn test_extract_via_rel_with_other_key_value_pairs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[sea_orm(belongs_to = "super::user::Entity", from = "user_id", to = "id", via_rel = "Author")]),
        ];
        assert_eq!(extract_via_rel(&attrs), Some("Author".to_string()));
    }

    #[test]
    fn test_extract_via_rel_multiple_sea_orm_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[sea_orm(has_many)]),
            syn::parse_quote!(#[sea_orm(via_rel = "Comments")]),
        ];
        assert_eq!(extract_via_rel(&attrs), Some("Comments".to_string()));
    }

    #[test]
    fn test_extract_sea_orm_default_value_float() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = 0.7)])];
        assert_eq!(
            extract_sea_orm_default_value(&attrs),
            Some("0.7".to_string())
        );
    }

    #[test]
    fn test_extract_sea_orm_default_value_int() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = 42)])];
        assert_eq!(
            extract_sea_orm_default_value(&attrs),
            Some("42".to_string())
        );
    }

    #[test]
    fn test_extract_sea_orm_default_value_string() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(default_value = "active")])];
        assert_eq!(
            extract_sea_orm_default_value(&attrs),
            Some("active".to_string())
        );
    }

    #[test]
    fn test_extract_sea_orm_default_value_bool() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = true)])];
        assert_eq!(
            extract_sea_orm_default_value(&attrs),
            Some("true".to_string())
        );
    }

    #[test]
    fn test_extract_sea_orm_default_value_with_other_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[sea_orm(column_type = "Decimal(Some((10, 2)))", default_value = 0.7)]),
        ];
        assert_eq!(
            extract_sea_orm_default_value(&attrs),
            Some("0.7".to_string())
        );
    }

    #[test]
    fn test_extract_sea_orm_default_value_none() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(column_type = "Text")])];
        assert_eq!(extract_sea_orm_default_value(&attrs), None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_non_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
        assert_eq!(extract_sea_orm_default_value(&attrs), None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_empty_attrs() {
        assert_eq!(extract_sea_orm_default_value(&[]), None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_non_list_meta() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm])];
        assert_eq!(extract_sea_orm_default_value(&attrs), None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_empty_value_after_equals() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = )])];
        assert_eq!(extract_sea_orm_default_value(&attrs), None);
    }

    #[test]
    fn test_extract_sea_orm_default_value_no_default_value_key() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(primary_key, auto_increment)])];
        assert_eq!(extract_sea_orm_default_value(&attrs), None);
    }

    #[rstest]
    #[case("NOW()", true)]
    #[case("CURRENT_TIMESTAMP()", true)]
    #[case("UUID()", true)]
    #[case("gen_random_uuid()", true)]
    #[case("0.7", false)]
    #[case("42", false)]
    #[case("true", false)]
    #[case("draft", false)]
    #[case("active", false)]
    fn test_is_sql_function_default(#[case] value: &str, #[case] expected: bool) {
        assert_eq!(is_sql_function_default(value), expected);
    }

    #[test]
    fn test_has_sea_orm_primary_key_true() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(primary_key)])];
        assert!(has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_with_other_attrs() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(primary_key, default_value = "gen_random_uuid()")])];
        assert!(has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_false() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
        assert!(!has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
        assert!(!has_sea_orm_primary_key(&attrs));
    }

    #[test]
    fn test_has_sea_orm_primary_key_empty_attrs() {
        assert!(!has_sea_orm_primary_key(&[]));
    }

    #[test]
    fn test_has_sea_orm_primary_key_non_list_meta() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm = "something"] )];
        assert!(!has_sea_orm_primary_key(&attrs));
    }

    #[rstest]
    #[case::comma_in_string(quote::quote!(#[sea_orm(default_value = "a, b")]), Some("a, b"))]
    #[case::after_bool(
        quote::quote!(#[sea_orm(auto_increment = false, default_value = "x")]),
        Some("x")
    )]
    #[case::negative(quote::quote!(#[sea_orm(default_value = -5)]), Some("-5"))]
    #[case::float(quote::quote!(#[sea_orm(default_value = 0.5)]), Some("0.5"))]
    #[case::bool_literal(quote::quote!(#[sea_orm(default_value = true)]), Some("true"))]
    #[case::key_named_like_default(quote::quote!(#[sea_orm(column_name = "default_value")]), None)]
    #[case::list_entry(quote::quote!(#[sea_orm(indexed(unique), default_value = "1")]), Some("1"))]
    #[case::malformed(quote::quote!(#[sea_orm(default_value)]), None)]
    #[case::non_ident_key(quote::quote!(#[sea_orm(a::b, default_value = "2")]), Some("2"))]
    fn default_value_is_parsed_as_meta(
        #[case] attr: proc_macro2::TokenStream,
        #[case] expected: Option<&str>,
    ) {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#attr)];
        assert_eq!(extract_sea_orm_default_value(&attrs).as_deref(), expected);
    }

    #[rstest]
    #[case::flag(quote::quote!(#[sea_orm(primary_key)]), true)]
    #[case::explicit_true(quote::quote!(#[sea_orm(primary_key = true)]), true)]
    #[case::explicit_false(quote::quote!(#[sea_orm(primary_key = false)]), false)]
    #[case::not_auto_increment(quote::quote!(#[sea_orm(primary_key, auto_increment = false)]), false)]
    #[case::substring_only(quote::quote!(#[sea_orm(column_name = "primary_key")]), false)]
    fn primary_key_is_parsed_as_meta(
        #[case] attr: proc_macro2::TokenStream,
        #[case] expected: bool,
    ) {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#attr)];
        assert_eq!(has_sea_orm_primary_key(&attrs), expected);
        // `auto_increment = false` is still (part of) the primary key.
        assert_eq!(
            is_sea_orm_primary_key(&attrs),
            expected || attr.to_string().contains("auto_increment")
        );
    }

    // SeaORM's column forms name the model field (`Column::UserId`,
    // `UserId` -> `user_id`); composite lists are normalized per column.
    #[rstest]
    #[case::field("user_id", "user_id", false)]
    #[case::column("UserId", "user_id", false)]
    #[case::column_path("Column::UserId", "user_id", false)]
    #[case::raw("r#type", "type", false)]
    #[case::composite("(Column::UserId, post_id)", "(user_id, post_id)", true)]
    #[case::composite_spaced("( A , )", "(a)", true)]
    fn relation_columns_are_normalized(
        #[case] value: &str,
        #[case] expected: &str,
        #[case] composite: bool,
    ) {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = #value, to = #value)])];
        let from = extract_belongs_to_from_field(&attrs).unwrap();
        assert_eq!(from, expected);
        assert_eq!(extract_belongs_to_to_field(&attrs).unwrap(), expected);
        assert_eq!(is_composite_columns(&from), composite);
    }

    #[test]
    fn relation_values_survive_non_string_entries() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to = "super::user::Entity", on_delete = Cascade, from = "user_id", to = "id")]
        )];
        assert_eq!(
            extract_belongs_to_from_field(&attrs).as_deref(),
            Some("user_id")
        );
        assert_eq!(extract_belongs_to_to_field(&attrs).as_deref(), Some("id"));
    }
}
