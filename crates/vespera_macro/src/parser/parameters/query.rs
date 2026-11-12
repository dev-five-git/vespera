use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation, ParameterStyle},
    schema::{SchemaRef, SchemaType},
};

use super::shared::{
    convert_to_inline_schema, is_known_type, is_primitive_or_like, named_struct_fields,
    serde_field_name,
};
use crate::{
    parser::schema::{
        extract_default, extract_flatten, extract_rename_all, extract_serde_skip_flags,
        parse_struct_to_schema, parse_type_to_schema_ref,
    },
    schema_macro::type_utils::{is_map_type as utils_is_map_type, is_option_type, option_inner},
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
        style: None,
        explode: None,
    }])
}

/// One `in: query` parameter per deserialized field of a `Query<Struct>`.
///
/// Mirrors serde's input-side view of the struct: `#[serde(skip)]` /
/// `skip_deserializing` fields are never read from the query string, and
/// `#[serde(flatten)]` fields of a known struct contribute that struct's
/// fields (all optional when the flattened field is an `Option`). A flattened
/// non-struct (e.g. a `HashMap` catch-all) has no fixed names and is left out.
///
/// Array fields (`Vec<T>`, sets) are documented as `style: form, explode:
/// true` — repeated keys (`?tag=a&tag=b`). Note that axum's own `Query` uses
/// `serde_urlencoded`, which cannot deserialize repeated keys into a `Vec`;
/// use `axum_extra::extract::Query` for that wire shape.
pub(super) fn parse_query_struct_to_parameters(
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let mut parameters = Vec::new();
    let mut visiting = Vec::new();
    collect_query_struct_parameters(
        ty,
        false,
        known_schemas,
        struct_definitions,
        &mut visiting,
        &mut parameters,
    );
    (!parameters.is_empty()).then_some(parameters)
}

fn collect_query_struct_parameters(
    ty: &Type,
    force_optional: bool,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
    visiting: &mut Vec<String>,
    parameters: &mut Vec<Parameter>,
) {
    let Some(struct_item) = named_struct_fields(ty, struct_definitions) else {
        return;
    };
    // A (boxed) struct that flattens itself would recurse forever.
    let struct_name = struct_item.ident.to_string();
    if visiting.contains(&struct_name) {
        return;
    }
    visiting.push(struct_name);
    let rename_all = extract_rename_all(&struct_item.attrs);

    for field in &struct_item.fields {
        let skip_flags = extract_serde_skip_flags(&field.attrs);
        if skip_flags.skip || skip_flags.skip_deserializing {
            continue;
        }
        let is_optional = is_option_type(&field.ty);

        if extract_flatten(&field.attrs) {
            let flattened_ty = option_inner(&field.ty).unwrap_or(&field.ty);
            collect_query_struct_parameters(
                flattened_ty,
                force_optional || is_optional,
                known_schemas,
                struct_definitions,
                visiting,
                parameters,
            );
            continue;
        }

        // #[serde(default)] fields are optional in request inputs even
        // when the Rust type is non-Option (B4: request optional).
        let has_default = extract_default(&field.attrs).is_some();
        let mut field_schema =
            parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions);

        if let SchemaRef::Ref(ref_ref) = &field_schema
            && let Some(type_name) = ref_ref.ref_path.strip_prefix("#/components/schemas/")
            && let Some(struct_def) = struct_definitions.get(type_name)
            && let Ok(nested_struct_item) = syn::parse_str::<syn::ItemStruct>(struct_def)
        {
            let nested_schema =
                parse_struct_to_schema(&nested_struct_item, known_schemas, struct_definitions);
            field_schema = SchemaRef::Inline(Box::new(nested_schema));
        }

        let is_array = matches!(
            &field_schema,
            SchemaRef::Inline(schema) if schema.schema_type == Some(SchemaType::Array)
        );
        parameters.push(Parameter {
            name: serde_field_name(field, rename_all.as_deref()),
            r#in: ParameterLocation::Query,
            description: None,
            required: Some(!(force_optional || is_optional || has_default)),
            schema: Some(convert_to_inline_schema(field_schema, is_optional)),
            example: None,
            style: is_array.then_some(ParameterStyle::Form),
            explode: is_array.then_some(true),
        });
    }
    visiting.pop();
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

    /// `(name, required, style, explode)` of every query parameter.
    type ParamShape = (String, Option<bool>, Option<ParameterStyle>, Option<bool>);

    fn query_shapes(ty: &str, defs: &[(&'static str, &'static str)]) -> Vec<ParamShape> {
        let struct_definitions: HashMap<&str, &str> = defs.iter().copied().collect();
        let ty: Type = syn::parse_str(ty).unwrap();
        parse_query_struct_to_parameters(&ty, &HashSet::new(), &struct_definitions)
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.name, p.required, p.style, p.explode))
            .collect()
    }

    fn shape(name: &str, required: bool) -> ParamShape {
        (name.to_string(), Some(required), None, None)
    }

    fn array_shape(name: &str, required: bool) -> ParamShape {
        (
            name.to_string(),
            Some(required),
            Some(ParameterStyle::Form),
            Some(true),
        )
    }

    #[rstest::rstest]
    #[case::skips_non_deserialized_fields(
        "Q",
        vec![(
            "Q",
            r"pub struct Q {
                #[serde(skip)] pub internal: String,
                #[serde(skip_deserializing)] pub computed: i32,
                #[serde(skip_serializing)] pub input_only: i32,
                pub q: String,
            }",
        )],
        vec![shape("input_only", true), shape("q", true)]
    )]
    #[case::flatten_expands_known_struct(
        "Q",
        vec![
            ("Q", r"pub struct Q { pub q: String, #[serde(flatten)] pub page: Page }"),
            ("Page", r#"#[serde(rename_all = "camelCase")] pub struct Page { pub page_size: u32, pub cursor: Option<String> }"#),
        ],
        vec![shape("q", true), shape("pageSize", true), shape("cursor", false)]
    )]
    #[case::flatten_option_makes_fields_optional(
        "Q",
        vec![
            ("Q", r"pub struct Q { #[serde(flatten)] pub page: Option<Page> }"),
            ("Page", r"pub struct Page { pub page: u32 }"),
        ],
        vec![shape("page", false)]
    )]
    #[case::flatten_unknown_type_is_left_out(
        "Q",
        vec![(
            "Q",
            r"pub struct Q { pub q: String, #[serde(flatten)] pub rest: HashMap<String, String> }",
        )],
        vec![shape("q", true)]
    )]
    #[case::self_flatten_terminates(
        "Q",
        vec![("Q", r"pub struct Q { pub q: String, #[serde(flatten)] pub again: Q }")],
        vec![shape("q", true)]
    )]
    #[case::array_fields_use_form_explode(
        "Q",
        vec![(
            "Q",
            r"pub struct Q { pub tags: Vec<String>, pub ids: Option<HashSet<u32>>, pub one: u32 }",
        )],
        vec![array_shape("tags", true), array_shape("ids", false), shape("one", true)]
    )]
    fn query_struct_shapes(
        #[case] ty: &str,
        #[case] defs: Vec<(&'static str, &'static str)>,
        #[case] expected: Vec<ParamShape>,
    ) {
        assert_eq!(query_shapes(ty, &defs), expected);
    }

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

        struct_definitions.insert("TupleQuery", r"pub struct TupleQuery(i32);");
        let ty: Type = syn::parse_str("TupleQuery").unwrap();
        assert!(
            parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions).is_none()
        );

        struct_definitions.insert("EmptyQuery", r"pub struct EmptyQuery {}");
        let ty: Type = syn::parse_str("EmptyQuery").unwrap();
        assert!(
            parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions).is_none()
        );

        struct_definitions.insert("BrokenQuery", "pub struct BrokenQuery {");
        let ty: Type = syn::parse_str("BrokenQuery").unwrap();
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
    fn schema_ref_is_preserved_when_nested_definition_is_invalid() {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas = HashSet::new();
        struct_definitions.insert(
            "QueryWithBrokenNested",
            r"pub struct QueryWithBrokenNested { pub nested: BrokenNested }",
        );
        known_schemas.insert("BrokenNested");
        struct_definitions.insert("BrokenNested", "pub struct BrokenNested {");

        let ty: Type = syn::parse_str("QueryWithBrokenNested").unwrap();
        let params = parse_query_struct_to_parameters(&ty, &known_schemas, &struct_definitions)
            .expect("query should parse");
        assert!(matches!(params[0].schema, Some(SchemaRef::Ref(_))));
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

    #[test]
    fn query_extractor_rejects_missing_or_non_type_generic_and_non_path_struct() {
        let bare: Type = syn::parse_quote!(Query);
        assert!(parse_query_extractor("q", &bare, &HashSet::new(), &HashMap::new()).is_none());
        let lifetime: Type = syn::parse_quote!(Query<'static>);
        assert!(parse_query_extractor("q", &lifetime, &HashSet::new(), &HashMap::new()).is_none());
        let tuple: Type = syn::parse_quote!((String,));
        assert!(
            parse_query_struct_to_parameters(&tuple, &HashSet::new(), &HashMap::new()).is_none()
        );
    }
}
