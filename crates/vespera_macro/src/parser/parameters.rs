use std::collections::HashMap;

use syn::{FnArg, Pat, PatType, Type};
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::{Schema, SchemaRef, SchemaType},
};

use super::schema::{
    extract_field_rename, extract_rename_all, is_primitive_type, parse_struct_to_schema,
    parse_type_to_schema_ref_with_schemas, rename_field,
};

/// Analyze function parameter and convert to OpenAPI Parameter(s)
/// Returns None if parameter should be ignored (e.g., Query<HashMap<...>>)
/// Returns Some(Vec<Parameter>) with one or more parameters
pub fn parse_function_parameter(
    arg: &FnArg,
    path_params: &[String],
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<Vec<Parameter>> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { pat, ty, .. }) => {
            // Extract parameter name from pattern
            let param_name = match pat.as_ref() {
                Pat::Ident(ident) => ident.ident.to_string(),
                Pat::TupleStruct(tuple_struct) => {
                    // Handle Path(id) pattern
                    if tuple_struct.elems.len() == 1
                        && let Pat::Ident(ident) = &tuple_struct.elems[0]
                    {
                        ident.ident.to_string()
                    } else {
                        return None;
                    }
                }
                _ => return None,
            };

            // Check for Option<TypedHeader<T>> first
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if !path.segments.is_empty() {
                    let segment = path.segments.first().unwrap();
                    let ident_str = segment.ident.to_string();
                    
                    // Handle Option<TypedHeader<T>>
                    if ident_str == "Option" {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                            && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                            && let Type::Path(inner_type_path) = inner_ty
                            && !inner_type_path.path.segments.is_empty()
                        {
                            let inner_segment = inner_type_path.path.segments.last().unwrap();
                            let inner_ident_str = inner_segment.ident.to_string();
                            
                            if inner_ident_str == "TypedHeader" {
                                // TypedHeader always uses string schema regardless of inner type
                                return Some(vec![Parameter {
                                    name: param_name.replace("_", "-"),
                                    r#in: ParameterLocation::Header,
                                    description: None,
                                    required: Some(false),
                                    schema: Some(SchemaRef::Inline(Box::new(Schema::string()))),
                                    example: None,
                                }]);
                            }
                        }
                    }
                }
            }

            // Check for common Axum extractors first (before checking path_params)
            // Handle both Path<T> and vespera::axum::extract::Path<T> by checking the last segment
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if !path.segments.is_empty() {
                    // Check the last segment (handles both Path<T> and vespera::axum::extract::Path<T>)
                    let segment = path.segments.last().unwrap();
                    let ident_str = segment.ident.to_string();

                    match ident_str.as_str() {
                        "Path" => {
                            // Path<T> extractor - use path parameter name from route if available
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                // Check if inner type is a tuple (e.g., Path<(String, String, String)>)
                                if let Type::Tuple(tuple) = inner_ty {
                                    // For tuple types, extract parameters from path string
                                    let mut parameters = Vec::new();
                                    let tuple_elems = &tuple.elems;

                                    // Match tuple elements with path parameters
                                    for (idx, elem_ty) in tuple_elems.iter().enumerate() {
                                        if let Some(param_name) = path_params.get(idx) {
                                            parameters.push(Parameter {
                                                name: param_name.clone(),
                                                r#in: ParameterLocation::Path,
                                                description: None,
                                                required: Some(true),
                                                schema: Some(
                                                    parse_type_to_schema_ref_with_schemas(
                                                        elem_ty,
                                                        known_schemas,
                                                        struct_definitions,
                                                    ),
                                                ),
                                                example: None,
                                            });
                                        }
                                    }

                                    if !parameters.is_empty() {
                                        return Some(parameters);
                                    }
                                } else {
                                    // Single path parameter
                                    // If there's exactly one path parameter, use its name
                                    let name = if path_params.len() == 1 {
                                        path_params[0].clone()
                                    } else {
                                        // Otherwise use the parameter name from the pattern
                                        param_name
                                    };
                                    return Some(vec![Parameter {
                                        name,
                                        r#in: ParameterLocation::Path,
                                        description: None,
                                        required: Some(true),
                                        schema: Some(parse_type_to_schema_ref_with_schemas(
                                            inner_ty,
                                            known_schemas,
                                            struct_definitions,
                                        )),
                                        example: None,
                                    }]);
                                }
                            }
                        }
                        "Query" => {
                            // Query<T> extractor
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                // Check if it's HashMap or BTreeMap - ignore these
                                if is_map_type(inner_ty) {
                                    return None;
                                }

                                // Check if it's a struct - expand to individual parameters
                                if let Some(struct_params) = parse_query_struct_to_parameters(
                                    inner_ty,
                                    known_schemas,
                                    struct_definitions,
                                ) {
                                    return Some(struct_params);
                                }

                                // Check if it's a known type (primitive or known schema)
                                // If unknown, don't add parameter
                                if !is_known_type(inner_ty, known_schemas, struct_definitions) {
                                    return None;
                                }

                                // Otherwise, treat as single parameter
                                return Some(vec![Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Query,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref_with_schemas(
                                        inner_ty,
                                        known_schemas,
                                        struct_definitions,
                                    )),
                                    example: None,
                                }]);
                            }
                        }
                        "Header" => {
                            // Header<T> extractor
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                                && let Some(syn::GenericArgument::Type(inner_ty)) =
                                    args.args.first()
                            {
                                return Some(vec![Parameter {
                                    name: param_name.clone(),
                                    r#in: ParameterLocation::Header,
                                    description: None,
                                    required: Some(true),
                                    schema: Some(parse_type_to_schema_ref_with_schemas(
                                        inner_ty,
                                        known_schemas,
                                        struct_definitions,
                                    )),
                                    example: None,
                                }]);
                            }
                        }
                        "TypedHeader" => {
                            // TypedHeader<T> extractor (axum::TypedHeader)
                            // TypedHeader always uses string schema regardless of inner type
                            return Some(vec![Parameter {
                                name: param_name.replace("_", "-"),
                                r#in: ParameterLocation::Header,
                                description: None,
                                required: Some(true),
                                schema: Some(SchemaRef::Inline(Box::new(Schema::string()))),
                                example: None,
                            }]);
                        }
                        "Json" => {
                            // Json<T> extractor - this will be handled as RequestBody
                            return None;
                        }
                        _ => {}
                    }
                }
            }

            // Check if it's a path parameter (by name match) - for non-extractor cases
            if path_params.contains(&param_name) {
                return Some(vec![Parameter {
                    name: param_name.clone(),
                    r#in: ParameterLocation::Path,
                    description: None,
                    required: Some(true),
                    schema: Some(parse_type_to_schema_ref_with_schemas(
                        ty,
                        known_schemas,
                        struct_definitions,
                    )),
                    example: None,
                }]);
            }

            // Bare primitive without extractor is ignored (cannot infer location)
            None
        }
    }
}

fn is_map_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if !path.segments.is_empty() {
            let segment = path.segments.last().unwrap();
            let ident_str = segment.ident.to_string();
            return ident_str == "HashMap" || ident_str == "BTreeMap";
        }
    }
    false
}

fn is_known_type(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> bool {
    // Check if it's a primitive type
    if is_primitive_type(ty) {
        return true;
    }

    // Check if it's a known struct
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return false;
        }

        let segment = path.segments.last().unwrap();
        let ident_str = segment.ident.to_string();

        // Get type name (handle both simple and qualified paths)

        // Check if it's in struct_definitions or known_schemas
        if struct_definitions.contains_key(&ident_str) || known_schemas.contains_key(&ident_str) {
            return true;
        }

        // Check for generic types like Vec<T>, Option<T> - recursively check inner type
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
            match ident_str.as_str() {
                "Vec" | "Option" => {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        return is_known_type(inner_ty, known_schemas, struct_definitions);
                    }
                }
                _ => {}
            }
        }
    }

    false
}

/// Parse struct fields to individual query parameters
/// Returns None if the type is not a struct or cannot be parsed
fn parse_query_struct_to_parameters(
    ty: &Type,
    known_schemas: &HashMap<String, String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<Vec<Parameter>> {
    // Check if it's a known struct
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return None;
        }

        let segment = path.segments.last().unwrap();
        let ident_str = segment.ident.to_string();

        // Get type name (handle both simple and qualified paths)

        // Check if it's a known struct
        if let Some(struct_def) = struct_definitions.get(&ident_str)
            && let Ok(struct_item) = syn::parse_str::<syn::ItemStruct>(struct_def)
        {
            let mut parameters = Vec::new();

            // Extract rename_all attribute from struct
            let rename_all = extract_rename_all(&struct_item.attrs);

            if let syn::Fields::Named(fields_named) = &struct_item.fields {
                for field in &fields_named.named {
                    let rust_field_name = field
                        .ident
                        .as_ref()
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    // Check for field-level rename attribute first (takes precedence)
                    let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                        renamed
                    } else {
                        // Apply rename_all transformation if present
                        rename_field(&rust_field_name, rename_all.as_deref())
                    };

                    let field_type = &field.ty;

                    // Check if field is Option<T>
                    let is_optional = matches!(
                        field_type,
                        Type::Path(type_path)
                            if type_path
                                .path
                                .segments
                                .first()
                                .map(|s| s.ident == "Option")
                                .unwrap_or(false)
                    );

                    // Parse field type to schema (inline, not ref)
                    // For Query parameters, we need inline schemas, not refs
                    let mut field_schema = parse_type_to_schema_ref_with_schemas(
                        field_type,
                        known_schemas,
                        struct_definitions,
                    );

                    // Convert ref to inline if needed (Query parameters should not use refs)
                    // If it's a ref to a known struct, get the struct definition and inline it
                    if let SchemaRef::Ref(ref_ref) = &field_schema {
                        // Try to extract type name from ref path (e.g., "#/components/schemas/User" -> "User")
                        if let Some(type_name) =
                            ref_ref.ref_path.strip_prefix("#/components/schemas/")
                            && let Some(struct_def) = struct_definitions.get(type_name)
                            && let Ok(nested_struct_item) =
                                syn::parse_str::<syn::ItemStruct>(struct_def)
                        {
                            // Parse the nested struct to schema (inline)
                            let nested_schema = parse_struct_to_schema(
                                &nested_struct_item,
                                known_schemas,
                                struct_definitions,
                            );
                            field_schema = SchemaRef::Inline(Box::new(nested_schema));
                        }
                    }

                    // If it's Option<T>, make it nullable
                    let final_schema = if is_optional {
                        if let SchemaRef::Inline(mut schema) = field_schema {
                            schema.nullable = Some(true);
                            SchemaRef::Inline(schema)
                        } else {
                            // If still a ref, convert to inline object with nullable
                            SchemaRef::Inline(Box::new(Schema {
                                schema_type: Some(SchemaType::Object),
                                nullable: Some(true),
                                ..Schema::object()
                            }))
                        }
                    } else {
                        // If it's still a ref, convert to inline object
                        match field_schema {
                            SchemaRef::Ref(_) => {
                                SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                            }
                            SchemaRef::Inline(schema) => SchemaRef::Inline(schema),
                        }
                    };

                    let required = !is_optional;

                    parameters.push(Parameter {
                        name: field_name,
                        r#in: ParameterLocation::Query,
                        description: None,
                        required: Some(required),
                        schema: Some(final_schema),
                        example: None,
                    });
                }
            }

            if !parameters.is_empty() {
                return Some(parameters);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::collections::HashMap;
    use vespera_core::route::ParameterLocation;
    use serial_test::serial;
    #[rstest]
    #[case(
        "fn test(params: Path<(String, i32)>) {}",
        vec!["user_id".to_string(), "count".to_string()],
        vec![vec![ParameterLocation::Path, ParameterLocation::Path]]
    )]
    #[case(
        "fn show(Path(id): Path<i32>) {}",
        vec!["item_id".to_string()],               // path string name differs from pattern
        vec![vec![ParameterLocation::Path]]       // expect path param captured
    )]
    #[case(
        "fn test(Query(params): Query<HashMap<String, String>>) {}",
        vec![],
        vec![vec![]] // Query<HashMap<..>> is ignored
    )]
    #[case(
        "fn test(TypedHeader(user_agent): TypedHeader<UserAgent>, count: i32) {}",
        vec![],
        vec![
            vec![ParameterLocation::Header], // first arg (TypedHeader)
            vec![],                          // second arg (primitive, ignored)
        ]
    )]
    #[case(
        "fn test(TypedHeader(user_agent): TypedHeader<UserAgent>, content_type: Option<TypedHeader<ContentType>>, authorization: Option<TypedHeader<Authorization<Bearer>>>) {}",
        vec![],
        vec![
            vec![ParameterLocation::Header], // first arg (TypedHeader)
            vec![ParameterLocation::Header], // second arg (TypedHeader)
            vec![ParameterLocation::Header], // third arg (TypedHeader)
        ]
    )]
    #[case(
        "fn test(user_agent: TypedHeader<UserAgent>, count: i32) {}",
        vec![],
        vec![
            vec![ParameterLocation::Header], // first arg (TypedHeader)
            vec![],                          // second arg (primitive, ignored)
        ]
    )]
    #[serial]
    fn test_parse_function_parameter_cases(
        #[case] func_src: &str,
        #[case] path_params: Vec<String>,
        #[case] expected_locations: Vec<Vec<ParameterLocation>>,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        for (idx, arg) in func.sig.inputs.iter().enumerate() {
            use insta::assert_debug_snapshot;

            let result =
                parse_function_parameter(arg, &path_params, &HashMap::new(), &HashMap::new());
            let expected = expected_locations
                .get(idx)
                .unwrap_or_else(|| expected_locations.last().unwrap());

            if expected.is_empty() {
                assert!(
                    result.is_none(),
                    "Expected None at arg index {}, func: {}",
                    idx,
                    func_src
                );
                continue;
            }

            let params = result.as_ref().expect("Expected Some parameters");
            let got_locs: Vec<ParameterLocation> = params.iter().map(|p| p.r#in.clone()).collect();
            assert_eq!(
                got_locs, *expected,
                "Location mismatch at arg index {idx}, func: {func_src}"
            );
            assert_debug_snapshot!(params);
        }
    }
}

