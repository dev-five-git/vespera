//! `OpenAPI` document generator

use std::collections::{BTreeMap, HashMap};

use vespera_core::{
    openapi::{Info, OpenApi, OpenApiVersion, Server, Tag},
    schema::{Components, SecurityScheme},
};

use crate::{metadata::CollectedMetadata, route_impl::StoredRouteInfo};

mod component_schemas;
mod defaults;
mod paths;
#[cfg(test)]
mod tests_accuracy;

use component_schemas::{
    build_file_cache, build_schema_lookups, build_struct_file_index, parse_component_schemas,
};
pub use defaults::extract_default_value_from_function;
#[cfg(test)]
pub use defaults::find_function_in_file;
use paths::build_path_items;
pub use paths::find_duplicate_operation_id;

/// OpenAPI security data parsed from the `vespera!` macro.
#[derive(Default)]
pub struct OpenApiSecurity {
    pub security_schemes: Option<BTreeMap<String, SecurityScheme>>,
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    pub tag_descriptions: Option<HashMap<String, String>>,
}

/// Generate `OpenAPI` document from collected metadata.
///
/// When `file_cache` is provided (from collector), skips file I/O entirely.
/// When `None`, falls back to reading files from disk (used in tests).
#[cfg(test)]
pub fn generate_openapi_doc_with_metadata(
    title: Option<String>,
    version: Option<String>,
    servers: Option<Vec<Server>>,
    security_config: Option<OpenApiSecurity>,
    metadata: &CollectedMetadata,
    file_cache: Option<HashMap<String, syn::File>>,
    route_storage: &[StoredRouteInfo],
) -> OpenApi {
    try_generate_openapi_doc_with_metadata(
        title,
        version,
        servers,
        security_config,
        metadata,
        file_cache,
        route_storage,
    )
    .expect("vespera: OpenAPI generation failed")
}

/// Fallible OpenAPI document generation used by proc-macro entry points so
/// worker diagnostics become compile errors instead of panics.
pub fn try_generate_openapi_doc_with_metadata(
    title: Option<String>,
    version: Option<String>,
    servers: Option<Vec<Server>>,
    security_config: Option<OpenApiSecurity>,
    metadata: &CollectedMetadata,
    file_cache: Option<HashMap<String, syn::File>>,
    route_storage: &[StoredRouteInfo],
) -> syn::Result<OpenApi> {
    let profiling = crate::env_access::var("VESPERA_PROFILE").is_ok();
    let mut stage_start = std::time::Instant::now();
    let mut stage = |name: &str| {
        if profiling {
            eprintln!(
                "[vespera-profile]     openapi {name}: {:?}",
                stage_start.elapsed()
            );
            stage_start = std::time::Instant::now();
        }
    };

    // Recursive generic instantiations materialise as components while the
    // schemas and paths are built; start from a clean slate (a previous
    // expansion on this thread may have left some behind).
    crate::parser::schema::generic_instances::take_materialized();
    let (known_schema_names, struct_definitions) = build_schema_lookups(metadata);
    let file_cache = file_cache.unwrap_or_else(|| build_file_cache(metadata));
    let struct_file_index = build_struct_file_index(&file_cache);
    stage("lookups + file index");
    // `{Name}Input` request views (installed for this generation only). A
    // candidate whose built request view turns out identical to its response
    // view is dropped and the components rebuilt, until every remaining input
    // really differs (dropping one can make a referencing type identical).
    let mut input_names = component_schemas::input_names(metadata, &known_schema_names);
    let mut schemas = loop {
        crate::parser::schema::generic_instances::take_materialized();
        crate::parser::schema::request_view::install(std::sync::Arc::clone(&input_names));
        let schemas = parse_component_schemas(
            metadata,
            &known_schema_names,
            &struct_definitions,
            &file_cache,
            &struct_file_index,
        )?;
        match component_schemas::without_identical_inputs(&input_names, &schemas) {
            Some(reduced) => input_names = reduced,
            None => break schemas,
        }
    };
    stage("component schemas");
    let (paths, all_tags) = build_path_items(
        metadata,
        &known_schema_names,
        &struct_definitions,
        &file_cache,
        route_storage,
    )?;
    stage("path items");
    for (name, schema) in crate::parser::schema::generic_instances::take_materialized() {
        schemas.entry(name).or_insert(schema);
    }
    let input_names = crate::parser::schema::request_view::snapshot();
    crate::parser::schema::request_view::install(std::sync::Arc::default());
    component_schemas::prune_unreferenced_inputs(&mut schemas, &paths, &input_names);
    let security_config = security_config.unwrap_or_default();
    let tags = build_tags(all_tags, security_config.tag_descriptions.as_ref());

    Ok(OpenApi {
        openapi: OpenApiVersion::V3_1_0,
        info: Info {
            title: title.unwrap_or_else(|| "API".to_string()),
            version: version.unwrap_or_else(|| "1.0.0".to_string()),
            ..Default::default()
        },
        // No configured server → omit `servers`: the OpenAPI default is the
        // same origin that serves the document, which is what an embedded
        // `/docs` UI needs. A hard-coded `http://localhost:3000` broke every
        // deployment that did not run on that exact address.
        servers,
        paths,
        components: Some(Components {
            schemas: if schemas.is_empty() {
                None
            } else {
                Some(schemas)
            },
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: security_config.security_schemes,
        }),
        security: security_config.security,
        tags,
        external_docs: None,
    })
}

/// Reject `#[route(security = [...])]` / top-level `security = [...]` names
/// that are not declared in `vespera!(security_schemes = [...])`.
///
/// Only enforced when the app declares `security_schemes` at all — an app that
/// relies on schemes supplied elsewhere (none declared here) is left alone.
/// An undeclared name produces a security requirement that points at nothing,
/// which OpenAPI tooling rejects and which silently disables auth in clients.
///
/// `schemes` must be the FINAL `components.securitySchemes` — i.e. after child
/// apps from `vespera!(merge = [...])` were merged in — so a route referencing
/// a scheme that only a merged child declares is accepted. The caller decides
/// whether validation applies at all (only when the app itself declared
/// `security_schemes`).
pub fn validate_security_references(
    metadata: &CollectedMetadata,
    schemes: &BTreeMap<String, SecurityScheme>,
    security: Option<&[BTreeMap<String, Vec<String>>]>,
) -> syn::Result<()> {
    let declared = || {
        schemes
            .keys()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let unknown = |user: String, name: &str| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "{user} references unknown security scheme `{name}`; declared in \
                 `vespera!(security_schemes = [...])`: {}",
                declared()
            ),
        )
    };
    for route in &metadata.routes {
        for name in route.security.iter().flatten() {
            if !schemes.contains_key(name) {
                let handler = if route.module_path.is_empty() {
                    route.function_name.clone()
                } else {
                    format!("{}::{}", route.module_path, route.function_name)
                };
                return Err(unknown(
                    format!(
                        "route `{handler}` (`{} {}`)",
                        route.method.to_ascii_uppercase(),
                        route.path
                    ),
                    name,
                ));
            }
        }
    }
    for requirement in security.into_iter().flatten() {
        for name in requirement.keys() {
            if !schemes.contains_key(name) {
                return Err(unknown("top-level `security`".to_string(), name));
            }
        }
    }
    Ok(())
}

fn build_tags(
    mut all_tags: std::collections::BTreeSet<String>,
    tag_descriptions: Option<&HashMap<String, String>>,
) -> Option<Vec<Tag>> {
    if let Some(descriptions) = tag_descriptions {
        all_tags.extend(descriptions.keys().cloned());
    }
    (!all_tags.is_empty()).then(|| {
        all_tags
            .into_iter()
            .map(|name| Tag {
                description: tag_descriptions
                    .and_then(|descriptions| descriptions.get(&name).cloned()),
                name,
                external_docs: None,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rstest::rstest;
    use vespera_core::schema::{SecurityScheme, SecuritySchemeType};

    use super::*;
    use crate::metadata::{CollectedMetadata, RouteMetadata, StructMetadata};

    #[test]
    fn empty_metadata_uses_openapi_defaults() {
        let metadata = CollectedMetadata::new();

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert_eq!(doc.openapi, OpenApiVersion::V3_1_0);
        assert_eq!(doc.info.title, "API");
        assert_eq!(doc.info.version, "1.0.0");
        assert!(doc.paths.is_empty());
        assert!(doc.components.as_ref().unwrap().schemas.is_none());
        // No configured server → `servers` is omitted (spec default: same origin).
        assert!(doc.servers.is_none());
    }

    #[rstest]
    #[case::defaults(None, None, "API", "1.0.0")]
    #[case::custom_title(Some("My API".to_string()), None, "My API", "1.0.0")]
    #[case::custom_version(None, Some("2.0.0".to_string()), "API", "2.0.0")]
    #[case::custom_both(
        Some("Test API".to_string()),
        Some("3.0.0".to_string()),
        "Test API",
        "3.0.0",
    )]
    fn title_version_cases(
        #[case] title: Option<String>,
        #[case] version: Option<String>,
        #[case] expected_title: &str,
        #[case] expected_version: &str,
    ) {
        let metadata = CollectedMetadata::new();

        let doc =
            generate_openapi_doc_with_metadata(title, version, None, None, &metadata, None, &[]);

        assert_eq!(doc.info.title, expected_title);
        assert_eq!(doc.info.version, expected_version);
    }

    #[test]
    fn explicit_servers_replace_default_server() {
        let metadata = CollectedMetadata::new();
        let servers = vec![
            Server {
                url: "https://api.example.com".to_string(),
                description: Some("Production".to_string()),
                variables: None,
            },
            Server {
                url: "http://localhost:3000".to_string(),
                description: Some("Development".to_string()),
                variables: None,
            },
        ];

        let doc = generate_openapi_doc_with_metadata(
            None,
            None,
            Some(servers),
            None,
            &metadata,
            None,
            &[],
        );

        let doc_servers = doc.servers.expect("servers present");
        assert_eq!(doc_servers.len(), 2);
        assert_eq!(doc_servers[0].url, "https://api.example.com");
        assert_eq!(doc_servers[1].url, "http://localhost:3000");
    }

    /// Route / top-level security names must exist in `security_schemes`
    /// (only enforced when schemes are declared at all).
    #[rstest::rstest]
    #[case::route_unknown("route_unknown", Some(vec!["bearerAuth", "apiKey"]), None, true, true)]
    #[case::global_unknown("global_unknown", None, Some("oauth"), true, true)]
    #[case::all_known("all_known", Some(vec!["bearerAuth"]), Some("bearerAuth"), true, false)]
    #[case::no_schemes_declared("no_schemes", Some(vec!["anything"]), None, false, false)]
    fn security_references_are_validated(
        #[case] name: &str,
        #[case] route_security: Option<Vec<&str>>,
        #[case] global: Option<&str>,
        #[case] declare_schemes: bool,
        #[case] is_error: bool,
    ) {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/secure".to_string(),
            function_name: "secure_route".to_string(),
            module_path: "routes::secure".to_string(),
            file_path: "virtual/secure.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: route_security.map(|names| names.into_iter().map(String::from).collect()),
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });
        let config = OpenApiSecurity {
            security_schemes: declare_schemes.then(|| {
                BTreeMap::from([(
                    "bearerAuth".to_string(),
                    SecurityScheme {
                        r#type: SecuritySchemeType::Http,
                        description: None,
                        name: None,
                        r#in: None,
                        scheme: Some("bearer".to_string()),
                        bearer_format: None,
                        flows: None,
                        open_id_connect_url: None,
                    },
                )])
            }),
            security: global.map(|g| vec![BTreeMap::from([(g.to_string(), Vec::new())])]),
            tag_descriptions: None,
        };
        let result = config.security_schemes.as_ref().map_or(Ok(()), |schemes| {
            validate_security_references(&metadata, schemes, config.security.as_deref())
        });
        assert_eq!(result.is_err(), is_error, "{name}");
        if let Err(error) = result {
            insta::assert_snapshot!(format!("unknown_security_scheme_{name}"), error.to_string());
        }
    }

    #[test]
    fn security_schemes_and_route_security_snapshot() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/secure".to_string(),
            function_name: "secure_route".to_string(),
            module_path: "routes::secure".to_string(),
            file_path: "virtual/secure.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["secure".to_string()]),
            security: Some(vec!["bearerAuth".to_string()]),
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: Some("A secured route".to_string()),
        });

        let security_schemes = BTreeMap::from([(
            "bearerAuth".to_string(),
            SecurityScheme {
                r#type: SecuritySchemeType::Http,
                description: Some("JWT bearer token".to_string()),
                name: None,
                r#in: None,
                scheme: Some("bearer".to_string()),
                bearer_format: Some("JWT".to_string()),
                flows: None,
                open_id_connect_url: None,
            },
        )]);
        let global_security = Some(vec![BTreeMap::from([(
            "bearerAuth".to_string(),
            Vec::new(),
        )])]);
        let route_storage = vec![StoredRouteInfo {
            fn_name: "secure_route".to_string(),
            method: Some("get".to_string()),
            custom_path: Some("/secure".to_string()),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["secure".to_string()]),
            security: Some(vec!["bearerAuth".to_string()]),
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: Some("A secured route".to_string()),
            file_path: None,
            fn_sig_str: "async fn secure_route() -> &'static str".to_string(),
        }];

        let doc = generate_openapi_doc_with_metadata(
            Some("Security API".to_string()),
            Some("1.0.0".to_string()),
            None,
            Some(OpenApiSecurity {
                security_schemes: Some(security_schemes),
                security: global_security,
                tag_descriptions: None,
            }),
            &metadata,
            None,
            &route_storage,
        );

        insta::assert_snapshot!(
            "openapi_security_schemes_and_route_security",
            serde_json::to_string_pretty(&doc).unwrap()
        );
    }

    #[test]
    fn multiple_security_schemes_are_serialized_in_sorted_order_snapshot() {
        let metadata = CollectedMetadata::new();
        let security_schemes = BTreeMap::from([
            (
                "zBearer".to_string(),
                SecurityScheme {
                    r#type: SecuritySchemeType::Http,
                    description: None,
                    name: None,
                    r#in: None,
                    scheme: Some("bearer".to_string()),
                    bearer_format: Some("JWT".to_string()),
                    flows: None,
                    open_id_connect_url: None,
                },
            ),
            (
                "apiKey".to_string(),
                SecurityScheme {
                    r#type: SecuritySchemeType::ApiKey,
                    description: Some("API key".to_string()),
                    name: Some("X-API-Key".to_string()),
                    r#in: Some("header".to_string()),
                    scheme: None,
                    bearer_format: None,
                    flows: None,
                    open_id_connect_url: None,
                },
            ),
            (
                "basicAuth".to_string(),
                SecurityScheme {
                    r#type: SecuritySchemeType::Http,
                    description: None,
                    name: None,
                    r#in: None,
                    scheme: Some("basic".to_string()),
                    bearer_format: None,
                    flows: None,
                    open_id_connect_url: None,
                },
            ),
        ]);

        let doc = generate_openapi_doc_with_metadata(
            Some("Security API".to_string()),
            Some("1.0.0".to_string()),
            None,
            Some(OpenApiSecurity {
                security_schemes: Some(security_schemes),
                security: None,
                tag_descriptions: None,
            }),
            &metadata,
            None,
            &[],
        );

        insta::assert_snapshot!(
            "openapi_security_schemes_sorted_order",
            serde_json::to_string_pretty(&doc).unwrap()
        );
    }

    #[test]
    fn route_operation_metadata_snapshot() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users/{id}".to_string(),
            function_name: "get_user".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "virtual/users.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: Some("getUser".to_string()),
            summary: Some("Get a user".to_string()),
            request_example: None,
            response_example: None,
            deprecated: true,
            description: None,
        });

        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_user".to_string(),
            method: Some("get".to_string()),
            custom_path: Some("/users/{id}".to_string()),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: Some("getUser".to_string()),
            summary: Some("Get a user".to_string()),
            request_example: None,
            response_example: None,
            deprecated: true,
            description: None,
            file_path: None,
            fn_sig_str: "async fn get_user() -> &'static str".to_string(),
        }];

        let doc = generate_openapi_doc_with_metadata(
            Some("Operation Metadata API".to_string()),
            Some("1.0.0".to_string()),
            None,
            None,
            &metadata,
            None,
            &route_storage,
        );

        insta::assert_snapshot!(
            "openapi_route_operation_metadata",
            serde_json::to_string_pretty(&doc).unwrap()
        );
    }

    #[test]
    fn typed_route_responses_snapshot() {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata::new(
            "NotFoundError".to_string(),
            "pub struct NotFoundError { pub message: String }".to_string(),
        ));
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users/{id}".to_string(),
            function_name: "get_user".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "virtual/users.rs".to_string(),
            error_status: Some(vec![404, 500]),
            typed_responses: Some(vec![(404, "NotFoundError".to_string())]),
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_user".to_string(),
            method: Some("get".to_string()),
            custom_path: Some("/users/{id}".to_string()),
            error_status: Some(vec![404, 500]),
            typed_responses: Some(vec![(404, "NotFoundError".to_string())]),
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            file_path: None,
            fn_sig_str: "async fn get_user() -> &'static str".to_string(),
        }];

        let doc = generate_openapi_doc_with_metadata(
            Some("Typed Responses API".to_string()),
            Some("1.0.0".to_string()),
            None,
            None,
            &metadata,
            None,
            &route_storage,
        );

        insta::assert_snapshot!(
            "openapi_typed_route_responses",
            serde_json::to_string_pretty(&doc).unwrap()
        );
    }

    #[test]
    fn route_headers_and_examples_snapshot() {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata::new(
            "User".to_string(),
            "pub struct User { pub name: String }".to_string(),
        ));
        metadata.routes.push(RouteMetadata {
            method: "post".to_string(),
            path: "/users".to_string(),
            function_name: "create_user".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "virtual/users.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            success_status: None,
            headers: vec![
                crate::metadata::HeaderParam {
                    name: "Authorization".to_string(),
                    required: true,
                    description: Some("Bearer token".to_string()),
                },
                crate::metadata::HeaderParam {
                    name: "X-Trace-Id".to_string(),
                    required: false,
                    description: None,
                },
            ],
            operation_id: None,
            summary: None,
            request_example: Some(serde_json::json!({ "name": "Alice" })),
            response_example: Some(serde_json::json!({ "name": "Alice" })),
            deprecated: false,
            description: None,
        });

        let route_storage = vec![StoredRouteInfo {
            fn_name: "create_user".to_string(),
            method: Some("post".to_string()),
            custom_path: Some("/users".to_string()),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            file_path: None,
            fn_sig_str: "async fn create_user(vespera::axum::Json(user): vespera::axum::Json<User>) -> vespera::axum::Json<User>".to_string(),
        }];

        let doc = generate_openapi_doc_with_metadata(
            Some("Headers API".to_string()),
            Some("1.0.0".to_string()),
            None,
            None,
            &metadata,
            None,
            &route_storage,
        );

        insta::assert_snapshot!(
            "openapi_route_headers_and_examples",
            serde_json::to_string_pretty(&doc).unwrap()
        );
    }

    #[test]
    fn tag_descriptions_snapshot() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "list_users".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "virtual/users.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });
        let route_storage = vec![StoredRouteInfo {
            fn_name: "list_users".to_string(),
            method: Some("get".to_string()),
            custom_path: Some("/users".to_string()),
            error_status: None,
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            file_path: None,
            fn_sig_str: "async fn list_users() -> &'static str".to_string(),
        }];

        let doc = generate_openapi_doc_with_metadata(
            Some("Tags API".to_string()),
            Some("1.0.0".to_string()),
            None,
            Some(OpenApiSecurity {
                security_schemes: None,
                security: None,
                tag_descriptions: Some(HashMap::from([
                    ("admin".to_string(), "Admin operations".to_string()),
                    ("users".to_string(), "User operations".to_string()),
                ])),
            }),
            &metadata,
            None,
            &route_storage,
        );

        insta::assert_snapshot!(
            "openapi_tag_descriptions",
            serde_json::to_string_pretty(&doc).unwrap()
        );
    }
}
