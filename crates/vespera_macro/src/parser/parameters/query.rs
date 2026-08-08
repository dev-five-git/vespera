use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::SchemaRef,
};

use super::shared::{convert_to_inline_schema, is_known_type, is_primitive_or_like};
use crate::{
    parser::schema::{
        extract_default, extract_field_rename, extract_rename_all, parse_struct_to_schema,
        parse_type_to_schema_ref, rename_field,
    },
    schema_macro::type_utils::{is_map_type as utils_is_map_type, is_option_type},
};

pub(super) fn parse_query_extractor(
    param_name: &str,
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Query" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };

    if utils_is_map_type(inner_ty) {
        return None;
    }
    if let Some(struct_params) =
        parse_query_struct_to_parameters(inner_ty, known_schemas, struct_definitions)
    {
        return Some(struct_params);
    }
    if is_primitive_or_like(inner_ty) || !is_known_type(inner_ty, known_schemas, struct_definitions)
    {
        return None;
    }

    Some(vec![Parameter {
        name: param_name.to_string(),
        r#in: ParameterLocation::Query,
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

pub(super) fn parse_query_struct_to_parameters(
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    // A segment-less path names no struct: fall through to `None` rather than panic.
    let ident_str = type_path.path.segments.last()?.ident.to_string();
    if let Some(struct_def) = struct_definitions.get(ident_str.as_str())
        && let Ok(struct_item) = syn::parse_str::<syn::ItemStruct>(struct_def)
    {
        let mut parameters = Vec::new();
        let rename_all = extract_rename_all(&struct_item.attrs);

        if let syn::Fields::Named(fields_named) = &struct_item.fields {
            for field in &fields_named.named {
                let rust_field_name = field
                    .ident
                    .as_ref()
                    .map_or_else(|| "unknown".to_string(), std::string::ToString::to_string);
                let field_name = extract_field_rename(&field.attrs)
                    .unwrap_or_else(|| rename_field(&rust_field_name, rename_all.as_deref()));
                let field_type = &field.ty;
                let is_optional = is_option_type(field_type);
                // #[serde(default)] fields are optional in request inputs even
                // when the Rust type is non-Option (B4: request optional).
                let has_default = extract_default(&field.attrs).is_some();
                let mut field_schema =
                    parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                if let SchemaRef::Ref(ref_ref) = &field_schema
                    && let Some(type_name) = ref_ref.ref_path.strip_prefix("#/components/schemas/")
                    && let Some(struct_def) = struct_definitions.get(type_name)
                    && let Ok(nested_struct_item) = syn::parse_str::<syn::ItemStruct>(struct_def)
                {
                    let nested_schema = parse_struct_to_schema(
                        &nested_struct_item,
                        known_schemas,
                        struct_definitions,
                    );
                    field_schema = SchemaRef::Inline(Box::new(nested_schema));
                }

                parameters.push(Parameter {
                    name: field_name,
                    r#in: ParameterLocation::Query,
                    description: None,
                    required: Some(!(is_optional || has_default)),
                    schema: Some(convert_to_inline_schema(field_schema, is_optional)),
                    example: None,
                });
            }
        }

        if !parameters.is_empty() {
            return Some(parameters);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use syn::Type;
    use vespera_core::{
        route::ParameterLocation,
        schema::{SchemaRef, SchemaType},
    };

    use super::*;
    use crate::parser::parameters::parse_function_parameter;

    #[test]
    fn parse_query_struct_to_parameters_cases() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashSet::new();

        struct_definitions.insert(
            "QueryParams",
            r#"#[serde(rename_all = "camelCase")]
            pub struct QueryParams {
                pub page: i32,
                #[serde(rename = "per_page")]
                pub limit: Option<i32>,
                pub search: String,
            }"#,
        );

        let ty: Type = syn::parse_str("QueryParams").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("query params should parse");
        assert_eq!(params.len(), 3);
        assert_eq!(params[0].name, "page");
        assert_eq!(params[0].r#in, ParameterLocation::Query);
        assert_eq!(params[1].name, "per_page");
        assert_eq!(params[1].r#in, ParameterLocation::Query);
        assert_eq!(params[2].name, "search");
        assert_eq!(params[2].r#in, ParameterLocation::Query);

        struct_definitions.insert("NestedQuery", r"pub struct NestedQuery { pub user: User }");
        struct_definitions.insert("User", r"pub struct User { pub id: i32 }");
        known_schemas.insert("User");

        let ty: Type = syn::parse_str("NestedQuery").unwrap();
        assert!(
            parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions).is_some()
        );
        let ty: Type = syn::parse_str("i32").unwrap();
        assert!(
            parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions).is_none()
        );
        let ty: Type = syn::parse_str("UnknownStruct").unwrap();
        assert!(
            parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions).is_none()
        );

        struct_definitions.insert(
            "OptionalQuery",
            r"pub struct OptionalQuery { pub required: i32, pub optional: Option<String> }",
        );
        let ty: Type = syn::parse_str("OptionalQuery").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("optional query should parse");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].required, Some(true));
        assert_eq!(params[1].required, Some(false));
    }

    #[test]
    fn query_single_non_struct_known_type() {
        let mut known_schemas = HashSet::new();
        known_schemas.insert("CustomId");
        let func: syn::ItemFn = syn::parse_str("fn test(id: Query<CustomId>) {}").unwrap();
        let path_params: Vec<String> = vec![];
        let path_param_set: HashSet<&str> = HashSet::new();

        for arg in &func.sig.inputs {
            let result = parse_function_parameter(
                arg,
                &path_params,
                &path_param_set,
                &known_schemas,
                &HashMap::<&str, &str>::new(),
            );
            assert!(result.is_some(), "Expected single Query parameter");
            let params = result.unwrap();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].r#in, ParameterLocation::Query);
        }
    }

    #[test]
    fn parse_query_struct_empty_path_segments() {
        use syn::punctuated::Punctuated;

        let ty = Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(),
            },
        });
        assert!(
            parse_query_struct_to_parameters(
                &ty,
                &HashSet::<&str>::new(),
                &HashMap::<&str, &str>::new()
            )
            .is_none()
        );
    }

    #[test]
    fn schema_ref_to_inline_conversion_optional() {
        let mut struct_definitions = HashMap::new();
        struct_definitions.insert(
            "QueryWithOptional",
            r"pub struct QueryWithOptional { pub count: Option<i32> }",
        );

        let ty: Type = syn::parse_str("QueryWithOptional").unwrap();
        let params =
            parse_query_struct_to_parameters(&ty, &HashSet::<&str>::new(), &struct_definitions)
                .expect("query should parse");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].required, Some(false));
        match &params[0].schema {
            Some(SchemaRef::Inline(schema)) => assert_eq!(schema.nullable, Some(true)),
            _ => panic!("Expected inline schema with nullable"),
        }
    }

    #[test]
    fn schema_ref_preserved_for_required_field() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashSet::new();
        struct_definitions.insert(
            "QueryWithRef",
            r"pub struct QueryWithRef { pub item: RefType }",
        );
        known_schemas.insert("RefType");

        let ty: Type = syn::parse_str("QueryWithRef").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("query should parse");
        match &params[0].schema {
            Some(SchemaRef::Ref(r)) => assert_eq!(r.ref_path, "#/components/schemas/RefType"),
            _ => panic!("Expected $ref schema for required known type"),
        }
    }

    #[test]
    fn schema_ref_converted_to_inline_with_struct_def() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashSet::new();
        struct_definitions.insert(
            "QueryWithNested",
            r"pub struct QueryWithNested { pub nested: NestedType }",
        );
        known_schemas.insert("NestedType");
        struct_definitions.insert("NestedType", r"pub struct NestedType { pub value: i32 }");

        let ty: Type = syn::parse_str("QueryWithNested").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("query should parse");
        assert!(matches!(params[0].schema, Some(SchemaRef::Inline(_))));
    }

    #[test]
    fn query_struct_with_enum_field_produces_ref() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashSet::new();
        struct_definitions.insert(
            "FilterParams",
            r"pub struct FilterParams { pub status: Status, pub page: i32 }",
        );
        known_schemas.insert("Status");
        struct_definitions.insert("Status", r"pub enum Status { Active, Inactive, Pending }");

        let ty: Type = syn::parse_str("FilterParams").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("query should parse");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "status");
        assert_eq!(params[0].r#in, ParameterLocation::Query);
        assert_eq!(params[0].required, Some(true));
        match &params[0].schema {
            Some(SchemaRef::Ref(r)) => assert_eq!(r.ref_path, "#/components/schemas/Status"),
            _ => panic!(
                "Expected $ref for enum query parameter, got: {:?}",
                params[0].schema
            ),
        }
        assert_eq!(params[1].name, "page");
        match &params[1].schema {
            Some(SchemaRef::Inline(s)) => assert_eq!(s.schema_type, Some(SchemaType::Integer)),
            _ => panic!("Expected inline integer schema"),
        }
    }

    #[test]
    fn query_struct_serde_default_field_is_optional() {
        // B4: #[serde(default)] makes a non-Option query field optional in
        // request inputs (it can be omitted; the server fills the default).
        let mut struct_definitions = HashMap::new();
        struct_definitions.insert(
            "Paged",
            r"pub struct Paged {
                #[serde(default)]
                pub page: i32,
                pub q: String,
            }",
        );
        let ty: Type = syn::parse_str("Paged").unwrap();
        let params =
            parse_query_struct_to_parameters(&ty, &HashSet::<&str>::new(), &struct_definitions)
                .expect("query should parse");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "page");
        assert_eq!(params[0].required, Some(false)); // default → optional
        assert_eq!(params[1].name, "q");
        assert_eq!(params[1].required, Some(true));
    }

    #[test]
    fn query_struct_with_optional_enum_field() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashSet::new();
        struct_definitions.insert(
            "FilterParams",
            r"pub struct FilterParams { pub status: Option<Status> }",
        );
        known_schemas.insert("Status");
        struct_definitions.insert("Status", r"pub enum Status { Active, Inactive }");

        let ty: Type = syn::parse_str("FilterParams").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("query should parse");
        assert_eq!(params[0].required, Some(false));
        match &params[0].schema {
            Some(SchemaRef::Inline(s)) => {
                assert_eq!(s.ref_path, Some("#/components/schemas/Status".to_string()));
                assert_eq!(s.nullable, Some(true));
            }
            _ => panic!("Expected inline schema with ref_path and nullable for Option<Enum>"),
        }
    }
}
