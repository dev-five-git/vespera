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
use crate::schema_macro::type_utils::{
    is_map_type as utils_is_map_type, is_primitive_like as utils_is_primitive_like,
};

/// Convert SchemaRef to inline schema for query parameters
/// Query parameters should always use inline schemas, not refs
/// Adds nullable flag if the field is optional
fn convert_to_inline_schema(field_schema: SchemaRef, is_optional: bool) -> SchemaRef {
    match field_schema {
        SchemaRef::Inline(mut schema) => {
            if is_optional {
                schema.nullable = Some(true);
            }
            SchemaRef::Inline(schema)
        }
        SchemaRef::Ref(_) => {
            let mut schema = Schema::new(SchemaType::Object);
            if is_optional {
                schema.nullable = Some(true);
            }
            SchemaRef::Inline(Box::new(schema))
        }
    }
}

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
                    if ident_str == "Option"
                        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
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
                                    // Allow only when exactly one path parameter is provided
                                    if path_params.len() != 1 {
                                        return None;
                                    }
                                    let name = path_params[0].clone();
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
                                if utils_is_map_type(inner_ty) {
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

                                // Ignore primitive-like query params (including Vec/Option of primitive)
                                if is_primitive_type(inner_ty) || utils_is_primitive_like(inner_ty)
                                {
                                    return None;
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
                                // Ignore primitive-like headers
                                if is_primitive_type(inner_ty) || utils_is_primitive_like(inner_ty)
                                {
                                    return None;
                                }
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
                        "Form" => {
                            // Form<T> extractor - handled as RequestBody
                            return None;
                        }
                        "TypedMultipart" => {
                            // TypedMultipart<T> extractor - handled as RequestBody
                            return None;
                        }
                        "Multipart" => {
                            // Raw Multipart extractor - handled as RequestBody
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
                    if let SchemaRef::Ref(ref_ref) = &field_schema
                        && let Some(type_name) =
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

                    let final_schema = convert_to_inline_schema(field_schema, is_optional);

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
    use std::collections::HashMap;

    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;
    use vespera_core::route::ParameterLocation;

    use super::*;

    fn setup_test_data(func_src: &str) -> (HashMap<String, String>, HashMap<String, String>) {
        let mut struct_definitions = HashMap::new();
        let known_schemas: HashMap<String, String> = HashMap::new();

        if func_src.contains("QueryParams") {
            struct_definitions.insert(
                "QueryParams".to_string(),
                r#"
                pub struct QueryParams {
                    pub page: i32,
                    pub limit: Option<i32>,
                }
                "#
                .to_string(),
            );
        }

        if func_src.contains("User") {
            struct_definitions.insert(
                "User".to_string(),
                r#"
                pub struct User {
                    pub id: i32,
                    pub name: String,
                }
                "#
                .to_string(),
            );
        }

        (known_schemas, struct_definitions)
    }

    #[rstest]
    #[case(
        "fn test(params: Path<(String, i32)>) {}",
        vec!["user_id".to_string(), "count".to_string()],
        vec![vec![ParameterLocation::Path, ParameterLocation::Path]],
        "path_tuple"
    )]
    #[case(
        "fn show(Path(id): Path<i32>) {}",
        vec!["item_id".to_string()],
        vec![vec![ParameterLocation::Path]],
        "path_single"
    )]
    #[case(
        "fn test(Query(params): Query<HashMap<String, String>>) {}",
        vec![],
        vec![vec![]],
        "query_hashmap"
    )]
    #[case(
        "fn test(TypedHeader(user_agent): TypedHeader<UserAgent>, count: i32) {}",
        vec![],
        vec![
            vec![ParameterLocation::Header],
            vec![],
        ],
        "typed_header_and_arg"
    )]
    #[case(
        "fn test(TypedHeader(user_agent): TypedHeader<UserAgent>, content_type: Option<TypedHeader<ContentType>>, authorization: Option<TypedHeader<Authorization<Bearer>>>) {}",
        vec![],
        vec![
            vec![ParameterLocation::Header],
            vec![ParameterLocation::Header],
            vec![ParameterLocation::Header],
        ],
        "typed_header_multi"
    )]
    #[case(
        "fn test(user_agent: TypedHeader<UserAgent>, count: i32) {}",
        vec![],
        vec![
            vec![ParameterLocation::Header],
            vec![],
        ],
        "header_value_and_arg"
    )]
    #[case(
        "fn test(&self, id: i32) {}",
        vec![],
        vec![
            vec![],
            vec![],
        ],
        "method_receiver"
    )]
    #[case(
        "fn test(Path((a, b)): Path<(i32, String)>) {}",
        vec![],
        vec![vec![]],
        "path_tuple_destructure"
    )]
    #[case(
        "fn test(params: Query<QueryParams>) {}",
        vec![],
        vec![vec![ParameterLocation::Query, ParameterLocation::Query]],
        "query_struct"
    )]
    #[case(
        "fn test(body: Json<User>) {}",
        vec![],
        vec![vec![]],
        "json_body"
    )]
    #[case(
        "fn test(params: Query<UnknownType>) {}",
        vec![],
        vec![vec![]],
        "query_unknown"
    )]
    #[case(
        "fn test(params: Query<BTreeMap<String, String>>) {}",
        vec![],
        vec![vec![]],
        "query_map"
    )]
    #[case(
        "fn test(user: Query<User>) {}",
        vec![],
        vec![vec![ParameterLocation::Query, ParameterLocation::Query]],
        "query_user"
    )]
    #[case(
        "fn test(custom: Header<CustomHeader>) {}",
        vec![],
        vec![vec![ParameterLocation::Header]],
        "header_custom"
    )]
    #[case(
        "fn test(input: Form<User>) {}",
        vec![],
        vec![vec![]],
        "form_body"
    )]
    #[case(
        "fn test(upload: TypedMultipart<UploadRequest>) {}",
        vec![],
        vec![vec![]],
        "typed_multipart_body"
    )]
    #[case(
        "fn test(multipart: Multipart) {}",
        vec![],
        vec![vec![]],
        "raw_multipart_body"
    )]
    fn test_parse_function_parameter_cases(
        #[case] func_src: &str,
        #[case] path_params: Vec<String>,
        #[case] expected_locations: Vec<Vec<ParameterLocation>>,
        #[case] suffix: &str,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        let (known_schemas, struct_definitions) = setup_test_data(func_src);
        let mut parameters = Vec::new();

        for (idx, arg) in func.sig.inputs.iter().enumerate() {
            let result =
                parse_function_parameter(arg, &path_params, &known_schemas, &struct_definitions);
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
            parameters.extend(params.clone());
        }
        with_settings!({ snapshot_suffix => format!("params_{}", suffix) }, {
            assert_debug_snapshot!(parameters);
        });
    }

    #[rstest]
    #[case(
        "fn test(id: Query<i32>) {}",
        vec![],
    )]
    #[case(
        "fn test(auth: Header<String>) {}",
        vec![],
    )]
    #[case(
        "fn test(params: Query<Vec<i32>>) {}",
        vec![],
    )]
    #[case(
        "fn test(params: Query<Option<String>>) {}",
        vec![],
    )]
    #[case(
        "fn test(Path([a]): Path<[i32; 1]>) {}",
        vec![],
    )]
    #[case(
        "fn test(id: Path<i32>) {}",
        vec!["user_id".to_string(), "post_id".to_string()],
    )]
    #[case(
        "fn test((x, y): (i32, i32)) {}",
        vec![],
    )]
    fn test_parse_function_parameter_wrong_cases(
        #[case] func_src: &str,
        #[case] path_params: Vec<String>,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        let (known_schemas, struct_definitions) = setup_test_data(func_src);

        // Provide custom types for header/query known schemas/structs
        let mut struct_definitions = struct_definitions;
        struct_definitions.insert(
            "User".to_string(),
            "pub struct User { pub id: i32 }".to_string(),
        );
        let mut known_schemas = known_schemas;
        known_schemas.insert(
            "CustomHeader".to_string(),
            "#/components/schemas/CustomHeader".to_string(),
        );

        for (idx, arg) in func.sig.inputs.iter().enumerate() {
            let result =
                parse_function_parameter(arg, &path_params, &known_schemas, &struct_definitions);
            assert!(
                result.is_none(),
                "Expected None at arg index {}, func: {}, got: {:?}",
                idx,
                func_src,
                result
            );
        }
    }

    #[rstest]
    #[case("String", true)]
    #[case("i32", true)]
    #[case("Vec<String>", true)]
    #[case("Option<bool>", true)]
    #[case("CustomType", false)]
    fn test_is_primitive_like_fn(#[case] type_str: &str, #[case] expected: bool) {
        let ty: Type = syn::parse_str(type_str).unwrap();
        let result = is_primitive_type(&ty) || utils_is_primitive_like(&ty);
        assert_eq!(result, expected, "type_str={}", type_str);
    }

    #[rstest]
    #[case("HashMap<String, String>", true)]
    #[case("BTreeMap<String, String>", true)]
    #[case("String", false)]
    #[case("Vec<i32>", false)]
    fn test_is_map_type(#[case] type_str: &str, #[case] expected: bool) {
        let ty: Type = syn::parse_str(type_str).unwrap();
        assert_eq!(utils_is_map_type(&ty), expected, "type_str={}", type_str);
    }

    #[rstest]
    #[case("i32", HashMap::new(), HashMap::new(), true)] // primitive type
    #[case(
        "User",
        HashMap::new(),
        {
            let mut map = HashMap::new();
            map.insert("User".to_string(), "pub struct User { id: i32 }".to_string());
            map
        },
        true
    )] // known struct
    #[case(
        "Product",
        {
            let mut map = HashMap::new();
            map.insert("Product".to_string(), "Product".to_string());
            map
        },
        HashMap::new(),
        true
    )] // known schema
    #[case("Vec<i32>", HashMap::new(), HashMap::new(), true)] // Vec<T> with known inner type
    #[case("Option<String>", HashMap::new(), HashMap::new(), true)] // Option<T> with known inner type
    #[case("UnknownType", HashMap::new(), HashMap::new(), false)] // unknown type
    fn test_is_known_type(
        #[case] type_str: &str,
        #[case] known_schemas: HashMap<String, String>,
        #[case] struct_definitions: HashMap<String, String>,
        #[case] expected: bool,
    ) {
        let ty: Type = syn::parse_str(type_str).unwrap();
        assert_eq!(
            is_known_type(&ty, &known_schemas, &struct_definitions),
            expected,
            "Type: {}",
            type_str
        );
    }

    #[test]
    fn test_parse_query_struct_to_parameters() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashMap::new();

        // Test with struct that has fields
        struct_definitions.insert(
            "QueryParams".to_string(),
            r#"
            #[serde(rename_all = "camelCase")]
            pub struct QueryParams {
                pub page: i32,
                #[serde(rename = "per_page")]
                pub limit: Option<i32>,
                pub search: String,
            }
            "#
            .to_string(),
        );

        let ty: Type = syn::parse_str("QueryParams").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);
        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.len(), 3);
        assert_eq!(params[0].name, "page");
        assert_eq!(params[0].r#in, ParameterLocation::Query);
        assert_eq!(params[1].name, "per_page");
        assert_eq!(params[1].r#in, ParameterLocation::Query);
        assert_eq!(params[2].name, "search");
        assert_eq!(params[2].r#in, ParameterLocation::Query);

        // Test with struct that has nested struct (ref to inline conversion)
        struct_definitions.insert(
            "NestedQuery".to_string(),
            r#"
            pub struct NestedQuery {
                pub user: User,
            }
            "#
            .to_string(),
        );
        struct_definitions.insert(
            "User".to_string(),
            r#"
            pub struct User {
                pub id: i32,
            }
            "#
            .to_string(),
        );
        known_schemas.insert("User".to_string(), "#/components/schemas/User".to_string());

        let ty: Type = syn::parse_str("NestedQuery").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);
        assert!(result.is_some());

        // Test with non-struct type
        let ty: Type = syn::parse_str("i32").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);
        assert!(result.is_none());

        // Test with unknown struct
        let ty: Type = syn::parse_str("UnknownStruct").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);
        assert!(result.is_none());

        // Test with struct that has Option<T> fields
        struct_definitions.insert(
            "OptionalQuery".to_string(),
            r#"
            pub struct OptionalQuery {
                pub required: i32,
                pub optional: Option<String>,
            }
            "#
            .to_string(),
        );

        let ty: Type = syn::parse_str("OptionalQuery").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);
        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].required, Some(true));
        assert_eq!(params[1].required, Some(false));
    }

    // ======== Tests for uncovered lines ========

    #[test]
    fn test_query_single_non_struct_known_type() {
        // Test line 128: Return single Query<T> parameter where T is a known non-primitive type
        // This should return a single parameter when Query<T> wraps a known type that's not primitive-like
        let mut known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        // Add a known type that's not a struct
        known_schemas.insert(
            "CustomId".to_string(),
            "#/components/schemas/CustomId".to_string(),
        );

        let func: syn::ItemFn = syn::parse_str("fn test(id: Query<CustomId>) {}").unwrap();
        let path_params: Vec<String> = vec![];

        for arg in func.sig.inputs.iter() {
            let result =
                parse_function_parameter(arg, &path_params, &known_schemas, &struct_definitions);
            // Line 128 returns Some(vec![Parameter...]) for single Query parameter
            assert!(result.is_some(), "Expected single Query parameter");
            let params = result.unwrap();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].r#in, ParameterLocation::Query);
        }
    }

    #[test]
    fn test_path_param_by_name_match() {
        // Test line 159: path param matched by name (non-extractor case)
        // When a parameter name matches a path param name directly without Path<T> extractor
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let func: syn::ItemFn = syn::parse_str("fn test(user_id: i32) {}").unwrap();
        let path_params = vec!["user_id".to_string()];

        for arg in func.sig.inputs.iter() {
            let result =
                parse_function_parameter(arg, &path_params, &known_schemas, &struct_definitions);
            // Line 159: path_params.contains(&param_name) returns true, so it creates a Path parameter
            assert!(result.is_some(), "Expected path parameter by name match");
            let params = result.unwrap();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].r#in, ParameterLocation::Path);
            assert_eq!(params[0].name, "user_id");
        }
    }

    #[test]
    fn test_is_known_type_empty_segments() {
        // Test line 209: empty path segments returns false
        // Create a Type::Path programmatically with empty segments
        use syn::punctuated::Punctuated;

        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        // Create Type::Path with empty segments
        let type_path = syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(), // Empty segments!
            },
        };
        let ty = Type::Path(type_path);

        // Tests: path.segments.is_empty() is true
        assert!(!is_known_type(&ty, &known_schemas, &struct_definitions));
    }

    #[test]
    fn test_is_known_type_non_vec_option_generic() {
        // Test line 230: non-Vec/Option generic type (like Result<T, E> or Box<T>)
        // The match at line 224-229 only handles Vec and Option
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        // Box<i32> has angle brackets but is not Vec or Option
        let ty: Type = syn::parse_str("Box<i32>").unwrap();
        // Line 230: the default case `_ => {}` is hit, returns false
        assert!(!is_known_type(&ty, &known_schemas, &struct_definitions));

        // Result<i32, String> also not handled
        let ty: Type = syn::parse_str("Result<i32, String>").unwrap();
        assert!(!is_known_type(&ty, &known_schemas, &struct_definitions));
    }

    #[test]
    fn test_parse_query_struct_empty_path_segments() {
        // Test line 245: empty path segments in parse_query_struct_to_parameters
        // Create a Type::Path programmatically with empty segments
        use syn::punctuated::Punctuated;

        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        // Create Type::Path with empty segments
        let type_path = syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(), // Empty segments!
            },
        };
        let ty = Type::Path(type_path);

        // Tests: path.segments.is_empty() is true
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);
        assert!(
            result.is_none(),
            "Empty path segments should return None (line 245)"
        );
    }

    #[test]
    fn test_schema_ref_to_inline_conversion_optional() {
        // Test line 313: SchemaRef::Ref converted to inline for Optional fields
        // This requires a field that:
        // 1. Is Option<T> where T is a known schema
        // 2. T is NOT in struct_definitions (so ref stays as Ref)
        // 3. field_schema is still Ref after the conversion attempt
        //
        // Note: parse_type_to_schema_ref_with_schemas for Option<RefType> may create
        // an inline schema wrapping the inner ref, not a direct Ref.
        // Line 313 is a defensive case that may be hard to hit in practice.
        let mut struct_definitions = HashMap::new();
        let known_schemas = HashMap::new();

        // Use a simple struct with Option<i32> to verify the optional handling works
        struct_definitions.insert(
            "QueryWithOptional".to_string(),
            r#"
            pub struct QueryWithOptional {
                pub count: Option<i32>,
            }
            "#
            .to_string(),
        );

        let ty: Type = syn::parse_str("QueryWithOptional").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);

        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].required, Some(false));
        match &params[0].schema {
            Some(SchemaRef::Inline(schema)) => {
                assert_eq!(schema.nullable, Some(true));
            }
            _ => panic!("Expected inline schema with nullable"),
        }
    }

    #[test]
    fn test_schema_ref_to_inline_conversion_required() {
        // Test line 318: SchemaRef::Ref converted to inline for required fields
        // This requires a field where:
        // 1. field_schema is SchemaRef::Ref
        // 2. is_optional is false
        // 3. The ref conversion at lines 294-304 fails (no struct_def)
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashMap::new();

        // Struct with required RefType field
        struct_definitions.insert(
            "QueryWithRef".to_string(),
            r#"
            pub struct QueryWithRef {
                pub item: RefType,
            }
            "#
            .to_string(),
        );

        // RefType is a known schema (will generate SchemaRef::Ref)
        // BUT we don't have its struct definition, so the conversion at 296-303 fails
        known_schemas.insert(
            "RefType".to_string(),
            "#/components/schemas/RefType".to_string(),
        );

        let ty: Type = syn::parse_str("QueryWithRef").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);

        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.len(), 1);
        // Line 318: Ref that couldn't be converted is turned into inline object
        match &params[0].schema {
            Some(SchemaRef::Inline(schema)) => {
                assert_eq!(schema.schema_type, Some(SchemaType::Object));
            }
            _ => panic!("Expected inline schema (converted from Ref)"),
        }
    }

    #[test]
    fn test_schema_ref_converted_to_inline_with_struct_def() {
        // Test lines 294-304: Ref IS converted when struct_def exists
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashMap::new();

        // Main struct with a field of type NestedType
        struct_definitions.insert(
            "QueryWithNested".to_string(),
            r#"
            pub struct QueryWithNested {
                pub nested: NestedType,
            }
            "#
            .to_string(),
        );

        // NestedType is both in known_schemas AND has a struct definition
        known_schemas.insert(
            "NestedType".to_string(),
            "#/components/schemas/NestedType".to_string(),
        );
        struct_definitions.insert(
            "NestedType".to_string(),
            r#"
            pub struct NestedType {
                pub value: i32,
            }
            "#
            .to_string(),
        );

        let ty: Type = syn::parse_str("QueryWithNested").unwrap();
        let result = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions);

        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.len(), 1);
        // Lines 294-304: Ref is converted to inline by parsing the nested struct
        match &params[0].schema {
            Some(SchemaRef::Inline(_)) => {
                // Successfully converted
            }
            _ => panic!("Expected inline schema (converted from Ref via struct_def)"),
        }
    }

    // Tests for convert_to_inline_schema helper function
    #[test]
    fn test_convert_to_inline_schema_inline_required() {
        let schema = SchemaRef::Inline(Box::new(Schema::string()));
        let result = convert_to_inline_schema(schema, false);
        match result {
            SchemaRef::Inline(s) => {
                assert_eq!(s.schema_type, Some(SchemaType::String));
                assert!(s.nullable.is_none());
            }
            _ => panic!("Expected Inline"),
        }
    }

    #[test]
    fn test_convert_to_inline_schema_inline_optional() {
        let schema = SchemaRef::Inline(Box::new(Schema::string()));
        let result = convert_to_inline_schema(schema, true);
        match result {
            SchemaRef::Inline(s) => {
                assert_eq!(s.schema_type, Some(SchemaType::String));
                assert_eq!(s.nullable, Some(true));
            }
            _ => panic!("Expected Inline"),
        }
    }

    #[test]
    fn test_convert_to_inline_schema_ref_required() {
        use vespera_core::schema::Reference;
        let schema = SchemaRef::Ref(Reference::schema("SomeType"));
        let result = convert_to_inline_schema(schema, false);
        match result {
            SchemaRef::Inline(s) => {
                assert_eq!(s.schema_type, Some(SchemaType::Object));
                assert!(s.nullable.is_none());
            }
            _ => panic!("Expected Inline"),
        }
    }

    #[test]
    fn test_convert_to_inline_schema_ref_optional() {
        use vespera_core::schema::Reference;
        let schema = SchemaRef::Ref(Reference::schema("SomeType"));
        let result = convert_to_inline_schema(schema, true);
        match result {
            SchemaRef::Inline(s) => {
                assert_eq!(s.schema_type, Some(SchemaType::Object));
                assert_eq!(s.nullable, Some(true));
            }
            _ => panic!("Expected Inline"),
        }
    }
}
