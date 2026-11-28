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
/// `(a, b)` of field names (see [`is_composite_columns`]). A column names
/// the field of `fields` (the relation's own model) SeaORM's derive turns
/// into it (`address_2_id` -> `Address2Id`), else its capitals are split.
pub fn extract_belongs_to_from_field(
    attrs: &[syn::Attribute],
    fields: &syn::Fields,
) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "from").map(|value| normalize_columns(&value, fields))
}

/// Extract the `to` field name (the referenced column) from a `sea_orm`
/// relation attribute, normalized like [`extract_belongs_to_from_field`].
pub fn extract_belongs_to_to_field(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "to")
        .map(|value| normalize_columns(&value, &syn::Fields::Unit))
}

/// The `to` value as written: a `via_rel` relation's `to` names a column of
/// the model holding the `HasMany`, normalized against its fields by
/// [`normalize_columns`] once that model is at hand.
pub fn extract_belongs_to_to_raw(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "to")
}

/// Whether a normalized `from` / `to` value lists several columns.
pub fn is_composite_columns(columns: &str) -> bool {
    columns.starts_with('(')
}

/// `Column::UserId` / `UserId` / `user_id` -> `user_id`; `(A, b)` ->
/// `(a, b)`. A value that is no column path is kept as written.
pub fn normalize_columns(value: &str, fields: &syn::Fields) -> String {
    let trimmed = value.trim();
    if let Some(list) = trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let columns: Vec<String> = list
            .split(',')
            .map(str::trim)
            .filter(|column| !column.is_empty())
            .map(|column| column_field_name(column, fields))
            .collect();
        return format!("({})", columns.join(", "));
    }
    column_field_name(trimmed, fields)
}

/// The model field behind one column (`Column::UserId` -> `user_id`): the
/// field of `fields` whose PascalCase form is the column (SeaORM's
/// `DeriveEntityModel` naming, so `Address2Id` is `address_2_id`), else the
/// column's capitals split (`address2_id`).
fn column_field_name(column: &str, fields: &syn::Fields) -> String {
    let last = column.rsplit("::").next().unwrap_or(column).trim();
    let last = last.strip_prefix("r#").unwrap_or(last);
    if !last.starts_with(|c: char| c.is_ascii_uppercase()) {
        return last.to_string();
    }
    let named = fields.iter().filter_map(|field| field.ident.as_ref());
    if let Some(field) = named
        .map(|ident| crate::parser::strip_raw_prefix_owned(ident.to_string()))
        .find(|name| crate::schema_macro::type_utils::snake_to_pascal_case(name) == last)
    {
        return field;
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

/// Extract the `via_rel` value from a `sea_orm` attribute, else SeaORM 2's
/// `relation_reverse` (a `self_ref` relation's `via_rel`; M3, round 18: it
/// was ignored, so the relation's own field was searched for the key).
pub fn extract_via_rel(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "via_rel")
        .or_else(|| extract_sea_orm_attr_value(attrs, "relation_reverse"))
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

/// The SQL function of SeaORM's `default_expr = "Expr::cust(\"NOW()\")"`, the
/// spelling of the database default `default_value = "NOW()"` gives the
/// serde / schema default code (DEFERRED-X1, round 40). Any other expression
/// (`Expr::current_timestamp()`, a literal `Expr::cust("'draft'")`) has no SQL
/// function text here: `None`.
pub fn extract_sea_orm_default_expr_sql(attrs: &[syn::Attribute]) -> Option<String> {
    let entries = sea_orm_entries(attrs);
    let text = string_value(entry(&entries, "default_expr")?.value.as_ref()?)?;
    let syn::Expr::Call(call) = syn::parse_str::<syn::Expr>(&text).ok()? else {
        return None;
    };
    let syn::Expr::Path(func) = call.func.as_ref() else {
        return None;
    };
    let is_cust = func
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .eq(["Expr", "cust"]);
    match call.args.first() {
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(sql),
            ..
        })) if is_cust && call.args.len() == 1 && is_sql_function_default(&sql.value()) => {
            Some(sql.value())
        }
        _ => None,
    }
}

/// Whether the column has a database default `omit_default` drops it for:
/// `default_value`, or SeaORM 2's `default_expr = ".."`. A SQL-function
/// `default_expr` also reaches the serde / schema default code as its SQL text
/// (`extract_sea_orm_default_expr_sql`, round 40; R42-SM4-D1).
pub fn has_sea_orm_database_default(attrs: &[syn::Attribute]) -> bool {
    extract_sea_orm_default_value(attrs).is_some()
        || entry(&sea_orm_entries(attrs), "default_expr").is_some_and(|entry| entry.value.is_some())
}

/// SQL functions a `default_value` may spell with arguments (`nextval('seq')`):
/// the name (schema qualifier dropped, lower-cased) must be one of these.
///
/// **A closed list, on purpose** (R20-SM3-02, widened R21-SM3-04): a call with
/// arguments whose name is not listed is a text literal (`Draft(WIP)`), so an
/// unlisted SQL function used with arguments is emitted as its literal text.
/// Extend the list when a real default needs it; it is not a parser.
const SQL_FUNCTION_NAMES: &[&str] = &[
    "clock_timestamp",
    "current_date",
    "current_schema",
    "current_setting",
    "current_time",
    "current_timestamp",
    "current_user",
    "gen_random_uuid",
    "localtime",
    "localtimestamp",
    "nextval",
    "now",
    "random",
    "session_user",
    "statement_timestamp",
    "transaction_timestamp",
    "txid_current",
    "uuid_generate_v1",
    "uuid_generate_v4",
];

/// Check if a `sea_orm(default_value)` is a SQL function call: a (possibly
/// schema-qualified) identifier followed by a parenthesised argument list —
/// `now()`, `gen_random_uuid()`, `nextval('seq')`.  A text default that merely
/// contains a parenthesis (`"N/A (unknown)"`, `":)"`, `"Active (new)"`,
/// `"Draft(WIP)"`) is a literal: a space before `(` only fits an empty call
/// (`now ()`), and a call with arguments must name one of
/// [`SQL_FUNCTION_NAMES`] (R20-SM3-02, round 20: `"Draft(WIP)"` used to be
/// taken as a function, so the column's literal default was dropped).
///
/// An EMPTY call (`Draft()`, `my_default()`) is a function for ANY name: the
/// closed list guards only calls with arguments, so a user-defined
/// no-argument SQL function is accepted (R23-SM3-03, deliberate).
pub fn is_sql_function_default(value: &str) -> bool {
    let value = value.trim();
    let Some((name, args)) = value.split_once('(') else {
        return false;
    };
    let spaced = name.ends_with(char::is_whitespace);
    let name = name.trim_end();
    let mut chars = name.chars();
    let well_formed = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && closes_only_at_end(args);
    if !well_formed {
        return false;
    }
    let empty_call = args.trim() == ")";
    let bare = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    empty_call || (!spaced && SQL_FUNCTION_NAMES.contains(&bare.as_str()))
}

/// Whether the argument text after a call's `(` is closed by its final `)`
/// alone (R33-SM4-L1): a depth scan where the call's own `(` is depth 1,
/// and no `)` outside a single-quoted literal may bring the depth to 0 before
/// the last character. `now(approx) (estimated)` closes at its first `)`, so
/// it is a literal, not a call.
fn closes_only_at_end(args: &str) -> bool {
    let mut depth = 1_usize;
    let mut in_quote = false;
    for (index, c) in args.char_indices() {
        match c {
            '\'' => in_quote = !in_quote,
            '(' if !in_quote => depth += 1,
            ')' if !in_quote => {
                depth -= 1;
                if depth == 0 {
                    return index + c.len_utf8() == args.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// Check if a field is a database-generated primary key, mirroring
/// SeaORM's `DeriveEntityModel`: `#[sea_orm(primary_key)]` is
/// auto-increment only when it is the model's single primary key
/// (`primary_key_count == 1`) and either says `auto_increment = true` or,
/// with no `auto_increment`, its type is not a `String` / `Uuid` (SeaORM
/// makes those natural keys `auto_increment = false`).
pub fn has_sea_orm_primary_key(
    attrs: &[syn::Attribute],
    ty: &syn::Type,
    primary_key_count: usize,
) -> bool {
    let entries = sea_orm_entries(attrs);
    if !is_primary_key_entry(&entries) || primary_key_count != 1 {
        return false;
    }
    entry(&entries, "auto_increment").map_or_else(
        || !is_natural_key_type(ty),
        |e| e.value.as_ref().is_none_or(is_true),
    )
}

/// SeaORM's `NOT_AUTO_INCRE_TYPE_SUFFIX`: a key type (`Option<..>`
/// unwrapped) whose tokens end in `String` / `Uuid`.
fn is_natural_key_type(ty: &syn::Type) -> bool {
    let tokens = quote::ToTokens::to_token_stream(ty)
        .to_string()
        .replace(' ', "");
    let inner = tokens
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(&tokens);
    inner.ends_with("String") || inner.ends_with("Uuid")
}

/// How many distinct fields of a model are (part of) its primary key; a
/// `#[cfg]` twin pair under one name counts once. A field that a statically
/// false `#[cfg]` removes is not a key (R21-SM3-03, round 21), and a tuple
/// field has no name to count.
pub fn primary_key_count(fields: &syn::Fields) -> usize {
    let mut names: Vec<String> = fields
        .iter()
        .filter(|field| {
            is_sea_orm_primary_key(&field.attrs)
                && !crate::schema_macro::cfg_eval::compiled_out(&field.attrs)
        })
        .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
        .collect();
    names.sort_unstable();
    names.dedup();
    names.len()
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
            extract_belongs_to_from_field(&attrs, &syn::Fields::Unit),
            Some("user_id".to_string())
        );
    }

    #[test]
    fn test_extract_belongs_to_from_field_without_from() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(belongs_to, to = "id")])];
        assert_eq!(
            extract_belongs_to_from_field(&attrs, &syn::Fields::Unit),
            None
        );
    }

    #[test]
    fn test_extract_belongs_to_from_field_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(skip)])];
        assert_eq!(
            extract_belongs_to_from_field(&attrs, &syn::Fields::Unit),
            None
        );
    }

    #[test]
    fn test_extract_belongs_to_from_field_empty_attrs() {
        assert_eq!(extract_belongs_to_from_field(&[], &syn::Fields::Unit), None);
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

    #[rstest::rstest]
    #[case::default_value(quote::quote!(#[sea_orm(default_value = 1)]), true)]
    #[case::default_expr(
        quote::quote!(#[sea_orm(default_expr = "Expr::current_timestamp()")]),
        true
    )]
    #[case::bare_default_expr(quote::quote!(#[sea_orm(default_expr)]), false)]
    #[case::none(quote::quote!(#[sea_orm(indexed)]), false)]
    fn test_has_sea_orm_database_default(
        #[case] attr: proc_macro2::TokenStream,
        #[case] expected: bool,
    ) {
        let attr: syn::Attribute = syn::parse_quote!(#attr);
        assert_eq!(has_sea_orm_database_default(&[attr]), expected);
    }

    /// DEFERRED-X1 (round 40): only `Expr::cust("<SQL function>")` gives SQL
    /// text the serde / schema default code reads; anything else gives none.
    #[rstest::rstest]
    #[case::cust_now(r#"#[sea_orm(default_expr = "Expr::cust(\"NOW()\")")]"#, Some("NOW()"))]
    #[case::cust_uuid(
        r#"#[sea_orm(default_expr = "Expr::cust(\"gen_random_uuid()\")")]"#,
        Some("gen_random_uuid()")
    )]
    #[case::cust_literal(r#"#[sea_orm(default_expr = "Expr::cust(\"'draft'\")")]"#, None)]
    #[case::current_timestamp(r#"#[sea_orm(default_expr = "Expr::current_timestamp()")]"#, None)]
    #[case::cust_two_args(r#"#[sea_orm(default_expr = "Expr::cust(\"NOW()\", 1)")]"#, None)]
    #[case::not_a_call(r#"#[sea_orm(default_expr = "NOW()")]"#, None)]
    #[case::not_a_call_expr(r#"#[sea_orm(default_expr = "CURRENT_TIMESTAMP")]"#, None)]
    #[case::not_a_string(r"#[sea_orm(default_expr = 1)]", None)]
    #[case::not_a_path_call(r#"#[sea_orm(default_expr = "(f)(\"NOW()\")")]"#, None)]
    #[case::bare_default_expr(r"#[sea_orm(default_expr)]", None)]
    #[case::default_value_only(r#"#[sea_orm(default_value = "NOW()")]"#, None)]
    fn test_extract_sea_orm_default_expr_sql(#[case] attr: &str, #[case] expected: Option<&str>) {
        let item: syn::ItemStruct = syn::parse_str(&format!("{attr} struct S;")).unwrap();
        assert_eq!(
            extract_sea_orm_default_expr_sql(&item.attrs).as_deref(),
            expected
        );
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
    #[case("now ()", true)]
    #[case("nextval('seq')", true)]
    #[case("pg_catalog.now()", true)]
    #[case("N/A (unknown)", false)]
    // A text value with a parenthesised note: a space before `(` only fits
    // an empty call (`now ()`), never an argument list.
    #[case("Active (new)", false)]
    #[case("Draft (WIP)", false)]
    // R20-SM3-02 (round 20): an unspaced note is a literal too; an argument
    // list needs a known SQL function name.
    #[case("Draft(WIP)", false)]
    #[case("Active(new)", false)]
    #[case("pg_catalog.nextval('seq')", true)]
    #[case("(none)", false)]
    #[case(":)", false)]
    #[case("now(", false)]
    #[case("9lives()", false)]
    // R21-SM3-04 (round 21): the widened list names the common ones.
    #[case("current_setting('app.tenant')", true)]
    #[case("txid_current()", true)]
    #[case("Draft(wip)", false)]
    // R23-SM3-03: an EMPTY call is taken as a function whatever its name, on
    // purpose: a user-defined no-argument SQL function (`my_default()`) has no
    // closed list to be checked against, and `Draft()` is not a text value a
    // column default would plausibly hold. Pinned here so it stays a decision.
    #[case("Draft()", true)]
    #[case("my_default()", true)]
    // R33-SM4-L1: the call's `)` must be the last character; an earlier one
    // closes the call and the rest is a literal note.
    #[case("now(approx) (estimated)", false)]
    #[case("now() (estimated)", false)]
    #[case("nextval('a(b)')", true)]
    #[case("nextval(')')", true)]
    #[case("nextval(')", false)]
    #[case("now(a(b))", true)]
    #[case("now(a(b)) (note)", false)]
    fn test_is_sql_function_default(#[case] value: &str, #[case] expected: bool) {
        assert_eq!(is_sql_function_default(value), expected);
    }

    #[test]
    fn test_has_sea_orm_primary_key_true() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(primary_key)])];
        assert!(has_sea_orm_primary_key(&attrs, &syn::parse_quote!(i32), 1));
    }

    #[test]
    fn test_has_sea_orm_primary_key_with_other_attrs() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(primary_key, default_value = "gen_random_uuid()")])];
        assert!(has_sea_orm_primary_key(&attrs, &syn::parse_quote!(i32), 1));
    }

    #[test]
    fn test_has_sea_orm_primary_key_false() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
        assert!(!has_sea_orm_primary_key(&attrs, &syn::parse_quote!(i32), 1));
    }

    #[test]
    fn test_has_sea_orm_primary_key_no_sea_orm_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
        assert!(!has_sea_orm_primary_key(&attrs, &syn::parse_quote!(i32), 1));
    }

    #[test]
    fn test_has_sea_orm_primary_key_empty_attrs() {
        assert!(!has_sea_orm_primary_key(&[], &syn::parse_quote!(i32), 1));
    }

    #[test]
    fn test_has_sea_orm_primary_key_non_list_meta() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm = "something"] )];
        assert!(!has_sea_orm_primary_key(&attrs, &syn::parse_quote!(i32), 1));
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
        assert_eq!(
            has_sea_orm_primary_key(&attrs, &syn::parse_quote!(i32), 1),
            expected
        );
        // `auto_increment = false` is still (part of) the primary key.
        assert_eq!(
            is_sea_orm_primary_key(&attrs),
            expected || attr.to_string().contains("auto_increment")
        );
    }

    // SeaORM's `DeriveEntityModel`: a String / Uuid key and every column of
    // a composite key are `auto_increment = false` unless said otherwise.
    #[rstest]
    #[case::integer(quote::quote!(#[sea_orm(primary_key)]), quote::quote!(i64), 1, true)]
    #[case::string(quote::quote!(#[sea_orm(primary_key)]), quote::quote!(String), 1, false)]
    #[case::uuid(quote::quote!(#[sea_orm(primary_key)]), quote::quote!(uuid::Uuid), 1, false)]
    #[case::optional_uuid(quote::quote!(#[sea_orm(primary_key)]), quote::quote!(Option<Uuid>), 1, false)]
    #[case::string_forced(
        quote::quote!(#[sea_orm(primary_key, auto_increment = true)]),
        quote::quote!(String),
        1,
        true
    )]
    #[case::bare_flag(quote::quote!(#[sea_orm(primary_key, auto_increment)]), quote::quote!(String), 1, true)]
    #[case::composite(quote::quote!(#[sea_orm(primary_key)]), quote::quote!(i32), 2, false)]
    #[case::composite_forced(
        quote::quote!(#[sea_orm(primary_key, auto_increment = true)]),
        quote::quote!(i32),
        2,
        false
    )]
    fn primary_key_auto_increment_follows_sea_orm(
        #[case] attr: proc_macro2::TokenStream,
        #[case] ty: proc_macro2::TokenStream,
        #[case] count: usize,
        #[case] expected: bool,
    ) {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#attr)];
        let ty: syn::Type = syn::parse_quote!(#ty);
        assert_eq!(has_sea_orm_primary_key(&attrs, &ty, count), expected);
    }

    #[test]
    fn primary_key_count_counts_distinct_names() {
        let fields: syn::FieldsNamed = syn::parse_quote!({
            #[cfg(a)]
            #[sea_orm(primary_key)]
            id: i32,
            #[cfg(not(a))]
            #[sea_orm(primary_key)]
            id: i64,
            #[sea_orm(primary_key, auto_increment = false)]
            other: i32,
            plain: i32,
        });
        assert_eq!(primary_key_count(&syn::Fields::Named(fields)), 2);
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
        let from = extract_belongs_to_from_field(&attrs, &syn::Fields::Unit).unwrap();
        assert_eq!(from, expected);
        assert_eq!(extract_belongs_to_to_field(&attrs).unwrap(), expected);
        assert_eq!(is_composite_columns(&from), composite);
    }

    // L15 (round 12): SeaORM names `address_2_id`'s column `Address2Id`; the
    // capital split (`address2_id`) missed the field, so a nullable FK was
    // read as required.
    #[rstest]
    #[case::digit_word("Column::Address2Id", "address_2_id")]
    #[case::composite("(Address2Id, UserId)", "(address_2_id, user_id)")]
    #[case::raw_field("Type", "type")]
    #[case::unknown_column("Column::OtherId", "other_id")]
    fn from_columns_match_model_fields(#[case] value: &str, #[case] expected: &str) {
        let fields: syn::FieldsNamed = syn::parse_quote!({
            pub id: i32,
            pub address_2_id: Option<i32>,
            pub user_id: i32,
            pub r#type: String,
        });
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[sea_orm(belongs_to, from = #value, to = "Id")])];
        let fields = syn::Fields::Named(fields);
        assert_eq!(
            extract_belongs_to_from_field(&attrs, &fields).as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn relation_values_survive_non_string_entries() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to = "super::user::Entity", on_delete = Cascade, from = "user_id", to = "id")]
        )];
        assert_eq!(
            extract_belongs_to_from_field(&attrs, &syn::Fields::Unit).as_deref(),
            Some("user_id")
        );
        assert_eq!(extract_belongs_to_to_field(&attrs).as_deref(), Some("id"));
    }

    /// Named fields of a `struct` item, for the key-count cases.
    fn named_fields(source: &str) -> syn::Fields {
        let item: syn::ItemStruct = syn::parse_str(source).unwrap();
        item.fields
    }

    // R21-SM3-03 (round 21): a key a statically false `#[cfg]` removes is not
    // counted; a `#[cfg]` twin under one name counts once; a tuple field has no
    // name and counts none.
    #[rstest]
    #[case::one_key("pub struct M { #[sea_orm(primary_key)] pub id: i32 }", 1)]
    #[case::inactive_key_not_counted(
        "pub struct M { #[sea_orm(primary_key)] pub id: i32, #[cfg(any())] #[sea_orm(primary_key)] pub legacy_key: i32 }",
        1
    )]
    #[case::active_second_key_counted(
        "pub struct M { #[sea_orm(primary_key)] pub id: i32, #[cfg(all())] #[sea_orm(primary_key)] pub legacy_key: i32 }",
        2
    )]
    #[case::cfg_twin_counts_once(
        "pub struct M { #[cfg(unix)] #[sea_orm(primary_key)] pub id: i32, #[cfg(not(unix))] #[sea_orm(primary_key)] pub id: i32 }",
        1
    )]
    // R31-SM4-L1: a key removed through `cfg_attr(p, cfg(q))` is not counted.
    #[case::cfg_attr_inactive_key_not_counted(
        "pub struct M { #[sea_orm(primary_key)] pub id: i32, #[cfg_attr(all(), cfg(any()))] #[sea_orm(primary_key)] pub legacy_id: i32 }",
        1
    )]
    #[case::cfg_attr_active_key_counted(
        "pub struct M { #[sea_orm(primary_key)] pub id: i32, #[cfg_attr(all(), cfg(all()))] #[sea_orm(primary_key)] pub legacy_id: i32 }",
        2
    )]
    #[case::cfg_attr_only_key_removed(
        "pub struct M { #[cfg_attr(all(), cfg(any()))] #[sea_orm(primary_key)] pub id: i32 }",
        0
    )]
    #[case::tuple_field_counts_none("pub struct M(#[sea_orm(primary_key)] pub i32);", 0)]
    fn primary_key_count_skips_statically_inactive_keys(
        #[case] source: &str,
        #[case] expected: usize,
    ) {
        assert_eq!(primary_key_count(&named_fields(source)), expected);
    }
}
