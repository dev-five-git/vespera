/// Extract a named string value from a `sea_orm` attribute.
fn extract_sea_orm_attr_value(attrs: &[syn::Attribute], attr_name: &str) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("sea_orm") {
            return None;
        }

        let mut found_value = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(attr_name) {
                found_value = meta
                    .value()
                    .ok()
                    .and_then(|v| v.parse::<syn::LitStr>().ok())
                    .map(|lit| lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                drop(
                    meta.value()
                        .and_then(syn::parse::ParseBuffer::parse::<syn::LitStr>),
                );
            }
            Ok(())
        });
        found_value
    })
}

/// Extract the `from` field name from a `sea_orm` relation attribute.
pub fn extract_belongs_to_from_field(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "from")
}

/// Extract the `relation_enum` value from a `sea_orm` attribute.
pub fn extract_relation_enum(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "relation_enum")
}

/// Extract the `via_rel` value from a `sea_orm` attribute.
pub fn extract_via_rel(attrs: &[syn::Attribute]) -> Option<String> {
    extract_sea_orm_attr_value(attrs, "via_rel")
}

/// Extract `default_value` from a `sea_orm` attribute.
pub fn extract_sea_orm_default_value(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }

        let syn::Meta::List(meta_list) = &attr.meta else {
            continue;
        };
        let tokens = meta_list.tokens.to_string();

        if let Some(start) = tokens.find("default_value") {
            let remaining = &tokens[start + "default_value".len()..];
            let remaining = remaining.trim_start();
            if let Some(after_eq) = remaining.strip_prefix('=') {
                let value_str = after_eq.trim_start();
                let end = value_str.find(',').unwrap_or(value_str.len());
                let raw_value = value_str[..end].trim();

                if raw_value.is_empty() {
                    continue;
                }

                if let Some(inner) = raw_value
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                {
                    return Some(inner.to_string());
                }
                return Some(raw_value.to_string());
            }
        }
    }
    None
}

/// Check if a `sea_orm(default_value)` is a SQL function.
pub fn is_sql_function_default(value: &str) -> bool {
    value.contains('(')
}

/// Check if a field has `#[sea_orm(primary_key)]`.
pub fn has_sea_orm_primary_key(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }
        let syn::Meta::List(meta_list) = &attr.meta else {
            continue;
        };
        if meta_list.tokens.to_string().contains("primary_key") {
            return true;
        }
    }
    false
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
}
