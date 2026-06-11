use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::route::{Parameter, ParameterLocation};

use crate::parser::schema::parse_type_to_schema_ref_with_schemas;

pub(super) fn parse_path_extractor(
    ty: &Type,
    path_params: &[String],
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
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

    if let Type::Tuple(tuple) = inner_ty {
        let parameters = tuple
            .elems
            .iter()
            .enumerate()
            .filter_map(|(idx, elem_ty)| {
                path_params.get(idx).map(|param_name| Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Path,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref_with_schemas(
                        elem_ty,
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                })
            })
            .collect::<Vec<_>>();
        return (!parameters.is_empty()).then_some(parameters);
    }

    (path_params.len() == 1).then(|| {
        vec![Parameter {
            name: path_params[0].clone(),
            r#in: ParameterLocation::Path,
            description: None,
            required: Some(true),
            schema: Some(parse_type_to_schema_ref_with_schemas(
                inner_ty,
                known_schemas,
                struct_definitions,
            )),
            example: None,
        }]
    })
}

pub(super) fn parse_bare_path_parameter(
    param_name: &str,
    ty: &Type,
    path_param_set: &HashSet<String>,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<Vec<Parameter>> {
    path_param_set.contains(param_name).then(|| {
        vec![Parameter {
            name: param_name.to_string(),
            r#in: ParameterLocation::Path,
            description: None,
            required: Some(true),
            schema: Some(parse_type_to_schema_ref_with_schemas(
                ty,
                known_schemas,
                struct_definitions,
            )),
            example: None,
        }]
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use vespera_core::route::ParameterLocation;

    use crate::parser::parameters::parse_function_parameter;

    #[test]
    fn path_param_by_name_match() {
        let func: syn::ItemFn = syn::parse_str("fn test(user_id: i32) {}").unwrap();
        let path_params = vec!["user_id".to_string()];
        let path_param_set: HashSet<String> = path_params.iter().cloned().collect();

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
}
