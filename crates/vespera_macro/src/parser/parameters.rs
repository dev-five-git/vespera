use std::collections::{HashMap, HashSet};

use syn::{FnArg, Pat, PatType};
use vespera_core::route::Parameter;

mod header;
mod path;
mod query;
mod shared;

/// Analyze function parameter and convert to OpenAPI parameter(s).
pub fn parse_function_parameter(
    arg: &FnArg,
    path_params: &[String],
    path_param_set: &HashSet<String>,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<Vec<Parameter>> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { pat, ty, .. }) => {
            let param_name = extract_param_name(pat.as_ref())?;

            if let Some(parameters) = header::parse_option_typed_header(&param_name, ty) {
                return Some(parameters);
            }
            if let Some(parameters) =
                path::parse_path_extractor(ty, path_params, known_schemas, struct_definitions)
            {
                return Some(parameters);
            }
            if let Some(parameters) =
                query::parse_query_extractor(&param_name, ty, known_schemas, struct_definitions)
            {
                return Some(parameters);
            }
            if let Some(parameters) =
                header::parse_header_extractor(&param_name, ty, known_schemas, struct_definitions)
            {
                return Some(parameters);
            }

            path::parse_bare_path_parameter(
                &param_name,
                ty,
                path_param_set,
                known_schemas,
                struct_definitions,
            )
        }
    }
}

fn extract_param_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(ident.ident.to_string()),
        Pat::TupleStruct(tuple_struct) if tuple_struct.elems.len() == 1 => {
            let Pat::Ident(ident) = &tuple_struct.elems[0] else {
                return None;
            };
            Some(ident.ident.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;
    use vespera_core::route::ParameterLocation;

    use super::*;

    fn setup_test_data(func_src: &str) -> (HashSet<String>, HashMap<String, String>) {
        let mut struct_definitions = HashMap::new();
        let mut known_schemas: HashSet<String> = HashSet::new();

        if func_src.contains("QueryParams") {
            known_schemas.insert("QueryParams".to_string());
            struct_definitions.insert(
                "QueryParams".to_string(),
                r"pub struct QueryParams { pub page: i32, pub limit: Option<i32> }".to_string(),
            );
        }

        if func_src.contains("User") {
            known_schemas.insert("User".to_string());
            struct_definitions.insert(
                "User".to_string(),
                r"pub struct User { pub id: i32, pub name: String }".to_string(),
            );
        }

        (known_schemas, struct_definitions)
    }

    #[rstest]
    #[case("fn test(params: Path<(String, i32)>) {}", vec!["user_id".to_string(), "count".to_string()], vec![vec![ParameterLocation::Path, ParameterLocation::Path]], "path_tuple")]
    #[case("fn show(Path(id): Path<i32>) {}", vec!["item_id".to_string()], vec![vec![ParameterLocation::Path]], "path_single")]
    #[case("fn test(Query(params): Query<HashMap<String, String>>) {}", vec![], vec![vec![]], "query_hashmap")]
    #[case("fn test(TypedHeader(user_agent): TypedHeader<UserAgent>, count: i32) {}", vec![], vec![vec![ParameterLocation::Header], vec![]], "typed_header_and_arg")]
    #[case("fn test(TypedHeader(user_agent): TypedHeader<UserAgent>, content_type: Option<TypedHeader<ContentType>>, authorization: Option<TypedHeader<Authorization<Bearer>>>) {}", vec![], vec![vec![ParameterLocation::Header], vec![ParameterLocation::Header], vec![ParameterLocation::Header]], "typed_header_multi")]
    #[case("fn test(user_agent: TypedHeader<UserAgent>, count: i32) {}", vec![], vec![vec![ParameterLocation::Header], vec![]], "header_value_and_arg")]
    #[case("fn test(&self, id: i32) {}", vec![], vec![vec![], vec![]], "method_receiver")]
    #[case("fn test(Path((a, b)): Path<(i32, String)>) {}", vec![], vec![vec![]], "path_tuple_destructure")]
    #[case("fn test(params: Query<QueryParams>) {}", vec![], vec![vec![ParameterLocation::Query, ParameterLocation::Query]], "query_struct")]
    #[case("fn test(body: Json<User>) {}", vec![], vec![vec![]], "json_body")]
    #[case("fn test(params: Query<UnknownType>) {}", vec![], vec![vec![]], "query_unknown")]
    #[case("fn test(params: Query<BTreeMap<String, String>>) {}", vec![], vec![vec![]], "query_map")]
    #[case("fn test(user: Query<User>) {}", vec![], vec![vec![ParameterLocation::Query, ParameterLocation::Query]], "query_user")]
    #[case("fn test(custom: Header<CustomHeader>) {}", vec![], vec![vec![ParameterLocation::Header]], "header_custom")]
    #[case("fn test(input: Form<User>) {}", vec![], vec![vec![]], "form_body")]
    #[case("fn test(upload: TypedMultipart<UploadRequest>) {}", vec![], vec![vec![]], "typed_multipart_body")]
    #[case("fn test(multipart: Multipart) {}", vec![], vec![vec![]], "raw_multipart_body")]
    fn parse_function_parameter_cases(
        #[case] func_src: &str,
        #[case] path_params: Vec<String>,
        #[case] expected_locations: Vec<Vec<ParameterLocation>>,
        #[case] suffix: &str,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        let (known_schemas, struct_definitions) = setup_test_data(func_src);
        let path_param_set: HashSet<String> = path_params.iter().cloned().collect();
        let mut parameters = Vec::new();

        for (idx, arg) in func.sig.inputs.iter().enumerate() {
            let result = parse_function_parameter(
                arg,
                &path_params,
                &path_param_set,
                &known_schemas,
                &struct_definitions,
            );
            let expected = expected_locations
                .get(idx)
                .unwrap_or_else(|| expected_locations.last().unwrap());

            if expected.is_empty() {
                assert!(
                    result.is_none(),
                    "Expected None at arg index {idx}, func: {func_src}"
                );
                continue;
            }

            let params = result.as_ref().expect("Expected Some parameters");
            let got_locs: Vec<ParameterLocation> = params.iter().map(|p| p.r#in).collect();
            assert_eq!(
                got_locs, *expected,
                "Location mismatch at arg index {idx}, func: {func_src}"
            );
            parameters.extend(params.clone());
        }
        with_settings!({ snapshot_path => "snapshots", snapshot_suffix => format!("params_{suffix}") }, {
            assert_debug_snapshot!(parameters);
        });
    }

    #[rstest]
    #[case("fn test(id: Query<i32>) {}", vec![])]
    #[case("fn test(auth: Header<String>) {}", vec![])]
    #[case("fn test(params: Query<Vec<i32>>) {}", vec![])]
    #[case("fn test(params: Query<Option<String>>) {}", vec![])]
    #[case("fn test(Path([a]): Path<[i32; 1]>) {}", vec![])]
    #[case("fn test(id: Path<i32>) {}", vec!["user_id".to_string(), "post_id".to_string()])]
    #[case("fn test((x, y): (i32, i32)) {}", vec![])]
    fn parse_function_parameter_wrong_cases(
        #[case] func_src: &str,
        #[case] path_params: Vec<String>,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        let (mut known_schemas, mut struct_definitions) = setup_test_data(func_src);
        struct_definitions.insert(
            "User".to_string(),
            "pub struct User { pub id: i32 }".to_string(),
        );
        known_schemas.insert("CustomHeader".to_string());
        let path_param_set: HashSet<String> = path_params.iter().cloned().collect();

        for (idx, arg) in func.sig.inputs.iter().enumerate() {
            let result = parse_function_parameter(
                arg,
                &path_params,
                &path_param_set,
                &known_schemas,
                &struct_definitions,
            );
            assert!(
                result.is_none(),
                "Expected None at arg index {idx}, func: {func_src}, got: {result:?}"
            );
        }
    }
}
