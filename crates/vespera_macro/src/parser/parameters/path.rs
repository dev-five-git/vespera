use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::{Schema, SchemaRef},
};

use super::shared::{input_rename_all, named_struct_fields, serde_field_name};
use crate::parser::schema::{extract_doc_comment, parse_type_to_schema_ref};

/// A required `in: path` parameter.
pub(in crate::parser) fn path_parameter(name: &str, schema: SchemaRef) -> Parameter {
    Parameter {
        name: name.to_string(),
        r#in: ParameterLocation::Path,
        description: None,
        required: Some(true),
        schema: Some(schema),
        example: None,
        style: None,
        explode: None,
    }
}

/// Inline `{type: string}` — the schema of a path segment whose Rust type is
/// unknown (no extractor, a missing tuple position, or a struct field that
/// does not exist).
pub(in crate::parser) fn string_path_schema() -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema::string()))
}

/// One `in: path` parameter per `{name}` of the route path, typed from the
/// handler's `Path<T>` extractor:
///
/// - `Path<(A, B)>`: positional — the n-th tuple element types the n-th
///   segment (missing positions fall back to `string`).
/// - `Path<Struct>` (a known named-field struct): each segment is typed by the
///   struct field with the same serde name (`rename` / `rename_all`
///   respected); unmatched segments fall back to `string`.
/// - any other `Path<T>`: `T` types the segment when the route has exactly
///   one; with several segments a scalar cannot match them, so each is
///   documented as `string`.
pub(in crate::parser) fn path_extractor_parameters(
    inner_ty: &Type,
    path_params: &[String],
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Vec<Parameter> {
    if let Type::Tuple(tuple) = inner_ty {
        return path_params
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let schema = tuple
                    .elems
                    .get(idx)
                    .map_or_else(string_path_schema, |elem| {
                        parse_type_to_schema_ref(elem, known_schemas, struct_definitions)
                    });
                path_parameter(name, schema)
            })
            .collect();
    }

    if let Some(struct_item) = named_struct_fields(inner_ty, struct_definitions) {
        let rename_all = input_rename_all(&struct_item.attrs);
        return path_params
            .iter()
            .map(|name| {
                let field = struct_item
                    .fields
                    .iter()
                    .find(|field| serde_field_name(field, rename_all.as_deref()) == *name);
                let schema = field.map_or_else(string_path_schema, |field| {
                    parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions)
                });
                // The field's doc comment documents the parameter.
                Parameter {
                    description: field.and_then(|field| extract_doc_comment(&field.attrs)),
                    ..path_parameter(name, schema)
                }
            })
            .collect();
    }

    if let [name] = path_params {
        return vec![path_parameter(
            name,
            parse_type_to_schema_ref(inner_ty, known_schemas, struct_definitions),
        )];
    }
    path_params
        .iter()
        .map(|name| path_parameter(name, string_path_schema()))
        .collect()
}

pub(super) fn parse_path_extractor(
    ty: &Type,
    path_params: &[String],
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Path" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    let parameters =
        path_extractor_parameters(inner_ty, path_params, known_schemas, struct_definitions);
    (!parameters.is_empty()).then_some(parameters)
}

pub(super) fn parse_bare_path_parameter(
    param_name: &str,
    ty: &Type,
    path_param_set: &HashSet<&str>,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    path_param_set.contains(param_name).then(|| {
        vec![path_parameter(
            param_name,
            parse_type_to_schema_ref(ty, known_schemas, struct_definitions),
        )]
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use vespera_core::route::ParameterLocation;

    use super::parse_path_extractor;
    use crate::parser::parameters::parse_function_parameter;

    #[test]
    fn path_param_by_name_match() {
        let func: syn::ItemFn = syn::parse_str("fn test(user_id: i32) {}").unwrap();
        let path_params = vec!["user_id".to_string()];
        let path_param_set: HashSet<&str> = path_params.iter().map(String::as_str).collect();

        for arg in &func.sig.inputs {
            let result = parse_function_parameter(
                arg,
                &path_params,
                &path_param_set,
                &HashSet::new(),
                &HashMap::new(),
            );
            assert!(result.is_some(), "Expected path parameter by name match");
            let params = result.unwrap();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].r#in, ParameterLocation::Path);
            assert_eq!(params[0].name, "user_id");
        }
    }

    #[test]
    fn tuple_extractor_shorter_than_path_falls_back_to_string() {
        let ty: syn::Type = syn::parse_quote!((u32,));
        let names = vec!["id".to_string(), "rest".to_string()];
        let params =
            super::path_extractor_parameters(&ty, &names, &HashSet::new(), &HashMap::new());
        let types: Vec<_> = params
            .iter()
            .map(|p| match &p.schema {
                Some(vespera_core::schema::SchemaRef::Inline(s)) => {
                    (p.name.as_str(), s.schema_type)
                }
                other => panic!("unexpected schema {other:?}"),
            })
            .collect();
        assert_eq!(
            types,
            vec![
                ("id", Some(vespera_core::schema::SchemaType::Integer)),
                ("rest", Some(vespera_core::schema::SchemaType::String)),
            ]
        );
    }

    #[test]
    fn path_extractor_rejects_missing_or_non_type_generic() {
        let bare: syn::Type = syn::parse_quote!(Path);
        assert!(parse_path_extractor(&bare, &[], &HashSet::new(), &HashMap::new()).is_none());
        let lifetime: syn::Type = syn::parse_quote!(Path<'static>);
        assert!(parse_path_extractor(&lifetime, &[], &HashSet::new(), &HashMap::new()).is_none());
    }
}
