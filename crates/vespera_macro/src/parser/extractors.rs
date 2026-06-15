use syn::{GenericArgument, PathArguments, Type};

/// If `ty` is `Validated<Inner>`, return `Inner`; otherwise return `ty`.
pub(super) fn unwrap_validated_type(ty: &Type) -> &Type {
    extractor_inner_type(ty, "Validated").unwrap_or(ty)
}

/// Return true when the type is a `Validated<...>` extractor wrapper.
pub(super) fn is_validated_type(ty: &Type) -> bool {
    extractor_inner_type(ty, "Validated").is_some()
}

/// Extract the first generic type argument from an extractor by final path segment.
pub(super) fn extractor_inner_type<'a>(ty: &'a Type, extractor: &str) -> Option<&'a Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != extractor {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    Some(inner_ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_validated_inner_extractor() {
        let ty: Type = syn::parse_str("vespera::Validated<axum::Json<User>>").unwrap();
        let inner = unwrap_validated_type(&ty);
        assert_eq!(quote::quote!(#inner).to_string(), "axum :: Json < User >");
    }
}
