use syn::Type;

/// Extract the first generic type argument from a type like `Option<T>` or `Vec<T>`.
pub(super) fn extract_inner_generic(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner.clone());
    }
    None
}

/// Check if a type matches `Option<T>`.
pub(super) fn is_option_type(ty: &Type) -> bool {
    matches_type_name(
        ty,
        &["Option", "std::option::Option", "core::option::Option"],
    )
}

/// Check if a type matches `Vec<T>`.
pub(super) fn is_vec_type(ty: &Type) -> bool {
    matches_type_name(ty, &["Vec", "std::vec::Vec"])
}

/// Check if a type's path matches any of the given names.
fn matches_type_name(ty: &Type, names: &[&str]) -> bool {
    let path = match ty {
        Type::Path(type_path) if type_path.qself.is_none() => &type_path.path,
        _ => return false,
    };
    let sig = path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    names.contains(&sig.as_str())
}

/// Strip leading `r#` from raw identifiers.
pub(super) fn strip_raw_prefix(s: &str) -> String {
    s.strip_prefix("r#").unwrap_or(s).to_string()
}

/// Parse a human-readable byte unit string into bytes.
///
/// Supports: `"10MiB"`, `"1GB"`, `"500KB"`, `"1024"`, `"unlimited"`.
pub(super) fn parse_byte_unit(s: &str) -> Option<usize> {
    let s = s.trim();

    // Binary and decimal suffixes, longest first to avoid prefix collisions
    let suffixes: &[(&str, usize)] = &[
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("KB", 1_000),
        ("B", 1),
    ];

    for (suffix, multiplier) in suffixes {
        if let Some(num_str) = s.strip_suffix(suffix) {
            return num_str.trim().parse::<usize>().ok().map(|n| n * multiplier);
        }
    }

    // Plain number (bytes)
    s.parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn test_parse_byte_unit() {
        assert_eq!(parse_byte_unit("10MiB"), Some(10 * 1024 * 1024));
        assert_eq!(parse_byte_unit("50MiB"), Some(50 * 1024 * 1024));
        assert_eq!(parse_byte_unit("1GB"), Some(1_000_000_000));
        assert_eq!(parse_byte_unit("500KB"), Some(500_000));
        assert_eq!(parse_byte_unit("1024"), Some(1024));
        assert_eq!(parse_byte_unit("0"), Some(0));
        assert_eq!(parse_byte_unit("invalid"), None);
    }

    #[test]
    fn test_parse_byte_unit_all_suffixes() {
        assert_eq!(parse_byte_unit("1GiB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_byte_unit("2KiB"), Some(2 * 1024));
        assert_eq!(parse_byte_unit("3MB"), Some(3_000_000));
        assert_eq!(parse_byte_unit("4B"), Some(4));
        assert_eq!(parse_byte_unit("  5MiB  "), Some(5 * 1024 * 1024));
    }

    #[test]
    fn test_strip_raw_prefix() {
        assert_eq!(strip_raw_prefix("r#type"), "type");
        assert_eq!(strip_raw_prefix("normal"), "normal");
    }

    #[test]
    fn test_extract_inner_generic_option() {
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        let inner = extract_inner_generic(&ty).unwrap();
        assert_eq!(quote!(#inner).to_string(), "String");
    }

    #[test]
    fn test_extract_inner_generic_vec() {
        let ty: syn::Type = syn::parse_str("Vec<i32>").unwrap();
        let inner = extract_inner_generic(&ty).unwrap();
        assert_eq!(quote!(#inner).to_string(), "i32");
    }

    #[test]
    fn test_extract_inner_generic_no_generics() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(extract_inner_generic(&ty).is_none());
    }

    #[test]
    fn test_extract_inner_generic_non_path() {
        let ty: syn::Type = syn::parse_str("(i32, String)").unwrap();
        assert!(extract_inner_generic(&ty).is_none());
    }

    #[test]
    fn test_is_option_type() {
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        assert!(is_option_type(&ty));
        let ty: syn::Type = syn::parse_str("std::option::Option<i32>").unwrap();
        assert!(is_option_type(&ty));
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(!is_option_type(&ty));
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_vec_type() {
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(is_vec_type(&ty));
        let ty: syn::Type = syn::parse_str("std::vec::Vec<i32>").unwrap();
        assert!(is_vec_type(&ty));
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        assert!(!is_vec_type(&ty));
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_vec_type(&ty));
    }

    #[test]
    fn test_matches_type_name_simple() {
        let ty: syn::Type = syn::parse_str("Option<i32>").unwrap();
        assert!(matches_type_name(&ty, &["Option"]));
        assert!(!matches_type_name(&ty, &["Vec"]));
    }

    #[test]
    fn test_matches_type_name_qualified() {
        let ty: syn::Type = syn::parse_str("std::option::Option<i32>").unwrap();
        assert!(matches_type_name(&ty, &["std::option::Option"]));
        assert!(!matches_type_name(&ty, &["Option"]));
    }

    #[test]
    fn test_matches_type_name_non_path() {
        let ty: syn::Type = syn::parse_str("(i32, String)").unwrap();
        assert!(!matches_type_name(&ty, &["Option", "Vec"]));
    }
}
