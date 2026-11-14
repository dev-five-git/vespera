use proc_macro2::TokenStream;
use syn::Type;

use crate::schema_macro::type_utils::resolve_type_to_absolute_path;

/// Convert a source field type for the generated struct: SeaORM date-time
/// aliases become chrono types and every name is re-rooted so it resolves
/// at the macro call site, at any nesting depth (`Option<Vec<T>>`,
/// `HashMap<K, V>`, ...). See [`resolve_type_to_absolute_path`].
pub fn convert_type_with_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    resolve_type_to_absolute_path(ty, source_module_path)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::date_time_with_time_zone("seaorm_to_chrono_tz", "DateTimeWithTimeZone")]
    #[case::date_time_utc("seaorm_to_chrono_utc", "DateTimeUtc")]
    #[case::date_time_local("seaorm_to_chrono_local", "DateTimeLocal")]
    #[case::non_path_reference("seaorm_to_chrono_ref_str", "&str")]
    #[case::regular_type_passthrough("seaorm_to_chrono_string", "String")]
    fn convert_seaorm_type_to_chrono_snapshot(#[case] snapshot_name: &str, #[case] input: &str) {
        let ty: syn::Type = syn::parse_str(input).unwrap();
        insta::assert_snapshot!(
            snapshot_name,
            convert_type_with_chrono(&ty, &[]).to_string()
        );
    }

    #[rstest]
    #[case::option_datetime("with_chrono_option_datetime", "Option<DateTimeWithTimeZone>")]
    #[case::vec_datetime("with_chrono_vec_datetime", "Vec<DateTimeWithTimeZone>")]
    #[case::plain_type_passthrough("with_chrono_plain_i32", "i32")]
    fn convert_type_with_chrono_snapshot(#[case] snapshot_name: &str, #[case] input: &str) {
        let ty: syn::Type = syn::parse_str(input).unwrap();
        insta::assert_snapshot!(
            snapshot_name,
            convert_type_with_chrono(&ty, &[]).to_string()
        );
    }

    #[test]
    fn test_convert_seaorm_type_to_chrono_empty_path() {
        let ty = syn::Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let tokens = convert_type_with_chrono(&ty, &[]);
        assert!(tokens.to_string().trim().is_empty());
    }

    #[rstest]
    #[case::field_data_generic(
        "FieldData<NamedTempFile>",
        "vespera :: multipart :: FieldData < vespera :: tempfile :: NamedTempFile >"
    )]
    #[case::field_data_bare("FieldData", "vespera :: multipart :: FieldData")]
    #[case::field_data_lifetime("FieldData<'a>", "vespera :: multipart :: FieldData < 'a >")]
    #[case::named_temp_file("NamedTempFile", "vespera :: tempfile :: NamedTempFile")]
    #[case::json_alias("Json", "vespera :: serde_json :: Value")]
    #[case::non_path("&str", "& str")]
    #[case::wrapper_without_arguments("Option", "Option")]
    fn convert_type_with_chrono_cases(#[case] input: &str, #[case] expected: &str) {
        let ty: syn::Type = syn::parse_str(input).unwrap();
        let module = ["crate".to_string(), "models".to_string(), "x".to_string()];
        assert_eq!(convert_type_with_chrono(&ty, &module).to_string(), expected);
    }

    // M2: nested wrappers used to be converted one level deep only — the
    // inner `DateTimeWithTimeZone` / local enum stayed unqualified.
    #[rstest]
    #[case::option_vec_datetime("nested_option_vec_datetime", "Option<Vec<DateTimeWithTimeZone>>")]
    #[case::option_vec_local("nested_option_vec_local", "Option<Vec<MemoStatus>>")]
    #[case::hash_map_local("nested_hash_map_local", "HashMap<String, Vec<MemoStatus>>")]
    #[case::tuple_array("nested_tuple_array", "([MemoStatus; 2], (Option<DateTimeUtc>,))")]
    #[case::reference("nested_reference", "&'static [MemoStatus]")]
    #[case::naive_aliases("nested_naive_aliases", "(DateTime, NaiveDate, DateTime<Utc>)")]
    fn nested_types_resolve_at_every_depth(#[case] snapshot_name: &str, #[case] input: &str) {
        let ty: syn::Type = syn::parse_str(input).unwrap();
        let module = [
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        insta::assert_snapshot!(
            snapshot_name,
            convert_type_with_chrono(&ty, &module).to_string()
        );
    }
}
