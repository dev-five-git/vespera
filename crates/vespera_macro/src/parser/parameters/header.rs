use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::{Schema, SchemaRef},
};

use super::shared::is_primitive_or_like;
use crate::parser::schema::parse_type_to_schema_ref;

pub(super) fn parse_option_typed_header(param_name: &str, ty: &Type) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.first()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(Type::Path(inner_type_path))) = args.args.first() else {
        return None;
    };
    let inner_segment = inner_type_path.path.segments.last()?;
    (inner_segment.ident == "TypedHeader").then(|| vec![typed_header_parameter(param_name, false)])
}

pub(super) fn parse_header_extractor(
    param_name: &str,
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    match segment.ident.to_string().as_str() {
        "Header" => parse_header(param_name, segment, known_schemas, struct_definitions),
        "TypedHeader" => Some(vec![typed_header_parameter(param_name, true)]),
        _ => None,
    }
}

fn parse_header(
    param_name: &str,
    segment: &syn::PathSegment,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    if is_primitive_or_like(inner_ty) {
        return None;
    }
    Some(vec![Parameter {
        name: param_name.to_string(),
        r#in: ParameterLocation::Header,
        description: None,
        required: Some(true),
        schema: Some(parse_type_to_schema_ref(
            inner_ty,
            known_schemas,
            struct_definitions,
        )),
        example: None,
    }])
}

fn typed_header_parameter(param_name: &str, required: bool) -> Parameter {
    Parameter {
        name: param_name.replace('_', "-"),
        r#in: ParameterLocation::Header,
        description: None,
        required: Some(required),
        schema: Some(SchemaRef::Inline(Box::new(Schema::string()))),
        example: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_option_and_header_generics_are_rejected() {
        let bare_option: Type = syn::parse_quote!(Option);
        assert!(parse_option_typed_header("x", &bare_option).is_none());
        let lifetime_option: Type = syn::parse_quote!(Option<'static>);
        assert!(parse_option_typed_header("x", &lifetime_option).is_none());

        let bare_header: Type = syn::parse_quote!(Header);
        assert!(
            parse_header_extractor("x", &bare_header, &HashSet::new(), &HashMap::new()).is_none()
        );
        let lifetime_header: Type = syn::parse_quote!(Header<'static>);
        assert!(
            parse_header_extractor("x", &lifetime_header, &HashSet::new(), &HashMap::new())
                .is_none()
        );
    }
}
