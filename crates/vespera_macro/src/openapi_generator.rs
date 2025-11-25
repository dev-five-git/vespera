//! OpenAPI document generator

use std::collections::BTreeMap;
use std::path::Path;
use vespera_core::{
    openapi::{Info, OpenApi, OpenApiVersion, Server},
    route::{HttpMethod, PathItem},
    schema::Components,
};

use crate::metadata::CollectedMetadata;
use crate::parser::build_operation_from_function;

/// Generate OpenAPI document from collected metadata
pub fn generate_openapi_doc_with_metadata(
    _folder_path: &Path,
    _folder_name: &str,
    title: Option<String>,
    version: Option<String>,
    metadata: &CollectedMetadata,
) -> OpenApi {
    let mut paths: BTreeMap<String, PathItem> = BTreeMap::new();

    // Process routes from metadata
    for route_meta in &metadata.routes {
        // Try to parse the file to get the actual function
        let content = match std::fs::read_to_string(&route_meta.file_path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!(
                    "Warning: Failed to read file {}: {}",
                    route_meta.file_path, e
                );
                continue;
            }
        };

        let file_ast = match syn::parse_file(&content) {
            Ok(ast) => ast,
            Err(e) => {
                eprintln!(
                    "Warning: Failed to parse file {}: {}",
                    route_meta.file_path, e
                );
                continue;
            }
        };

        for item in file_ast.items {
            if let syn::Item::Fn(fn_item) = item
                && fn_item.sig.ident == route_meta.function_name {
                    let method = HttpMethod::from(route_meta.method.as_str());

                    // Build operation from function signature
                    let operation = build_operation_from_function(&fn_item.sig, &route_meta.path);

                    // Get or create PathItem
                    let path_item =
                        paths
                            .entry(route_meta.path.clone())
                            .or_insert_with(|| PathItem {
                                get: None,
                                post: None,
                                put: None,
                                patch: None,
                                delete: None,
                                head: None,
                                options: None,
                                trace: None,
                                parameters: None,
                                summary: None,
                                description: None,
                            });

                    // Set operation for the method
                    path_item.set_operation(method, operation);
                    break;
                }
        }
    }

    // Build OpenAPI document
    OpenApi {
        openapi: OpenApiVersion::V3_1_0,
        info: Info {
            title: title.unwrap_or_else(|| "API".to_string()),
            version: version.unwrap_or_else(|| "1.0.0".to_string()),
            description: None,
            terms_of_service: None,
            contact: None,
            license: None,
            summary: None,
        },
        servers: Some(vec![Server {
            url: "http://localhost:3000".to_string(),
            description: None,
            variables: None,
        }]),
        paths,
        components: Some(Components {
            schemas: None,
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        }),
        security: None,
        tags: None,
        external_docs: None,
    }
}

