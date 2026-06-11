use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use crate::schema_macro::type_utils::resolve_type_to_absolute_path;

/// Convert `SeaORM` datetime types to chrono equivalents.
pub fn convert_seaorm_type_to_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let Type::Path(type_path) = ty else {
        return quote! { #ty };
    };

    let Some(segment) = type_path.path.segments.last() else {
        return quote! { #ty };
    };

    match segment.ident.to_string().as_str() {
        "DateTimeWithTimeZone" => {
            quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
        }
        "DateTimeUtc" => quote! { vespera::chrono::DateTime<vespera::chrono::Utc> },
        "DateTimeLocal" => quote! { vespera::chrono::DateTime<vespera::chrono::Local> },
        "FieldData" => convert_field_data(segment, source_module_path),
        "NamedTempFile" => quote! { vespera::tempfile::NamedTempFile },
        _ => resolve_type_to_absolute_path(ty, source_module_path),
    }
}

fn convert_field_data(segment: &syn::PathSegment, source_module_path: &[String]) -> TokenStream {
    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
        let inner_args: Vec<_> = args
            .args
            .iter()
            .map(|arg| {
                if let syn::GenericArgument::Type(inner_ty) = arg {
                    let converted = convert_seaorm_type_to_chrono(inner_ty, source_module_path);
                    quote! { #converted }
                } else {
                    quote! { #arg }
                }
            })
            .collect();
        quote! { vespera::multipart::FieldData<#(#inner_args),*> }
    } else {
        quote! { vespera::multipart::FieldData }
    }
}

/// Convert a type to chrono equivalent, handling `Option<T>` and `Vec<T>` wrappers.
pub fn convert_type_with_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    if let Some((wrapper, inner_ty)) = option_or_vec_inner(ty) {
        let converted_inner = convert_seaorm_type_to_chrono(inner_ty, source_module_path);
        return match wrapper {
            "Option" => quote! { Option<#converted_inner> },
            "Vec" => quote! { Vec<#converted_inner> },
            _ => unreachable!(),
        };
    }

    convert_seaorm_type_to_chrono(ty, source_module_path)
}

fn option_or_vec_inner(ty: &Type) -> Option<(&'static str, &Type)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.first()?;
    let wrapper = match segment.ident.to_string().as_str() {
        "Option" => "Option",
        "Vec" => "Vec",
        _ => return None,
    };
    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
    {
        Some((wrapper, inner_ty))
    } else {
        None
    }
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
            convert_seaorm_type_to_chrono(&ty, &[]).to_string()
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
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let tokens = convert_seaorm_type_to_chrono(&ty, &[]);
        assert!(tokens.to_string().is_empty() || tokens.to_string().trim().is_empty());
    }

    #[test]
    fn test_convert_seaorm_type_field_data_with_generic() {
        let ty: syn::Type = syn::parse_str("FieldData<NamedTempFile>").unwrap();
        let output = convert_seaorm_type_to_chrono(&ty, &[]).to_string();
        assert!(output.contains("vespera :: multipart :: FieldData"));
        assert!(output.contains("vespera :: tempfile :: NamedTempFile"));
    }

    #[test]
    fn test_convert_seaorm_type_field_data_without_generic() {
        let ty: syn::Type = syn::parse_str("FieldData").unwrap();
        let output = convert_seaorm_type_to_chrono(&ty, &[]).to_string();
        assert!(output.contains("vespera :: multipart :: FieldData"));
        assert!(!output.contains("NamedTempFile"));
    }

    #[test]
    fn test_convert_seaorm_type_field_data_with_non_type_generic() {
        let ty: syn::Type = syn::parse_str("FieldData<'a>").unwrap();
        let output = convert_seaorm_type_to_chrono(&ty, &[]).to_string();
        assert!(output.contains("vespera :: multipart :: FieldData"));
    }

    #[test]
    fn test_convert_seaorm_type_named_temp_file() {
        let ty: syn::Type = syn::parse_str("NamedTempFile").unwrap();
        let output = convert_seaorm_type_to_chrono(&ty, &[]).to_string();
        assert_eq!(output.trim(), "vespera :: tempfile :: NamedTempFile");
    }

    #[test]
    fn test_convert_type_with_chrono_json_alias_uses_public_value_path() {
        let ty: syn::Type = syn::parse_str("Json").unwrap();
        let tokens = convert_type_with_chrono(
            &ty,
            &[
                "crate".to_string(),
                "models".to_string(),
                "json_case".to_string(),
            ],
        );
        assert_eq!(tokens.to_string().trim(), "vespera :: serde_json :: Value");
    }
}
