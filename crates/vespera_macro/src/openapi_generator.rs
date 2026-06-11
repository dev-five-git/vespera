//! `OpenAPI` document generator

use std::collections::HashMap;

use vespera_core::{
    openapi::{Info, OpenApi, OpenApiVersion, Server, Tag},
    schema::Components,
};

use crate::{metadata::CollectedMetadata, route_impl::StoredRouteInfo};

mod component_schemas;
mod defaults;
mod paths;

use component_schemas::{
    build_file_cache, build_schema_lookups, build_struct_file_index, parse_component_schemas,
};
pub use defaults::{extract_default_value_from_function, find_function_in_file};
use paths::build_path_items;

/// Generate `OpenAPI` document from collected metadata.
///
/// When `file_cache` is provided (from collector), skips file I/O entirely.
/// When `None`, falls back to reading files from disk (used in tests).
pub fn generate_openapi_doc_with_metadata(
    title: Option<String>,
    version: Option<String>,
    servers: Option<Vec<Server>>,
    metadata: &CollectedMetadata,
    file_cache: Option<HashMap<String, syn::File>>,
    route_storage: &[StoredRouteInfo],
) -> OpenApi {
    let profiling = std::env::var("VESPERA_PROFILE").is_ok();
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

    let (known_schema_names, struct_definitions) = build_schema_lookups(metadata);
    let file_cache = file_cache.unwrap_or_else(|| build_file_cache(metadata));
    let struct_file_index = build_struct_file_index(&file_cache);
    stage("lookups + file index");
    let schemas = parse_component_schemas(
        metadata,
        &known_schema_names,
        &struct_definitions,
        &file_cache,
        &struct_file_index,
    );
    stage("component schemas");
    let (paths, all_tags) = build_path_items(
        metadata,
        &known_schema_names,
        &struct_definitions,
        &file_cache,
        route_storage,
    );
    stage("path items");

    OpenApi {
        openapi: OpenApiVersion::V3_1_0,
        info: Info {
            title: title.unwrap_or_else(|| "API".to_string()),
            version: version.unwrap_or_else(|| "1.0.0".to_string()),
            ..Default::default()
        },
        servers: servers.or_else(|| {
            Some(vec![Server {
                url: "http://localhost:3000".to_string(),
                description: None,
                variables: None,
            }])
        }),
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
            security_schemes: None,
        }),
        security: None,
        tags: if all_tags.is_empty() {
            None
        } else {
            Some(
                all_tags
                    .into_iter()
                    .map(|name| Tag {
                        name,
                        description: None,
                        external_docs: None,
                    })
                    .collect(),
            )
        },
        external_docs: None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::metadata::CollectedMetadata;

    #[test]
    fn empty_metadata_uses_openapi_defaults() {
        let metadata = CollectedMetadata::new();

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None, &[]);

        assert_eq!(doc.openapi, OpenApiVersion::V3_1_0);
        assert_eq!(doc.info.title, "API");
        assert_eq!(doc.info.version, "1.0.0");
        assert!(doc.paths.is_empty());
        assert!(doc.components.as_ref().unwrap().schemas.is_none());
        assert_eq!(doc.servers.as_ref().unwrap().len(), 1);
        assert_eq!(
            doc.servers.as_ref().unwrap()[0].url,
            "http://localhost:3000"
        );
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

        let doc = generate_openapi_doc_with_metadata(title, version, None, &metadata, None, &[]);

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

        let doc =
            generate_openapi_doc_with_metadata(None, None, Some(servers), &metadata, None, &[]);

        let doc_servers = doc.servers.expect("servers present");
        assert_eq!(doc_servers.len(), 2);
        assert_eq!(doc_servers[0].url, "https://api.example.com");
        assert_eq!(doc_servers[1].url, "http://localhost:3000");
    }
}
