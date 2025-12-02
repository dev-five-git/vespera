//! OpenAPI document generator

use std::collections::BTreeMap;
use vespera_core::{
    openapi::{Info, OpenApi, OpenApiVersion, Server},
    route::{HttpMethod, PathItem},
    schema::Components,
};

use crate::metadata::CollectedMetadata;
use crate::parser::{build_operation_from_function, parse_struct_to_schema};

/// Generate OpenAPI document from collected metadata
pub fn generate_openapi_doc_with_metadata(
    title: Option<String>,
    version: Option<String>,
    metadata: &CollectedMetadata,
) -> OpenApi {
    let mut paths: BTreeMap<String, PathItem> = BTreeMap::new();
    let mut schemas: BTreeMap<String, vespera_core::schema::Schema> = BTreeMap::new();
    let mut known_schema_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // First, collect all struct schemas
    for struct_meta in &metadata.structs {
        let schema = parse_struct_to_schema(
            &syn::parse_str(&struct_meta.definition).unwrap(),
            &known_schema_names,
        );
        let schema_name = struct_meta.name.clone();
        schemas.insert(schema_name.clone(), schema);
        known_schema_names.insert(schema_name.clone(), schema_name);
    }

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
                && fn_item.sig.ident == route_meta.function_name
            {
                let method = HttpMethod::from(route_meta.method.as_str());

                // Build operation from function signature
                let operation = build_operation_from_function(
                    &fn_item.sig,
                    &route_meta.path,
                    &known_schema_names,
                    route_meta.error_status.as_deref(),
                );

                // Get or create PathItem
                let path_item = paths
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
        tags: None,
        external_docs: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{CollectedMetadata, RouteMetadata, StructMetadata};
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.path().join(filename);
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    #[test]
    fn test_generate_openapi_empty_metadata() {
        let metadata = CollectedMetadata::new();

        let doc = generate_openapi_doc_with_metadata(None, None, &metadata);

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
    #[case(None, None, "API", "1.0.0")]
    #[case(Some("My API".to_string()), None, "My API", "1.0.0")]
    #[case(None, Some("2.0.0".to_string()), "API", "2.0.0")]
    #[case(Some("Test API".to_string()), Some("3.0.0".to_string()), "Test API", "3.0.0")]
    fn test_generate_openapi_title_version(
        #[case] title: Option<String>,
        #[case] version: Option<String>,
        #[case] expected_title: &str,
        #[case] expected_version: &str,
    ) {
        let metadata = CollectedMetadata::new();

        let doc = generate_openapi_doc_with_metadata(title, version, &metadata);

        assert_eq!(doc.info.title, expected_title);
        assert_eq!(doc.info.version, expected_version);
    }

    #[test]
    fn test_generate_openapi_with_route() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create a test route file
        let route_content = r#"
pub fn get_users() -> String {
    "users".to_string()
}
"#;
        let route_file = create_temp_file(&temp_dir, "users.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: route_file.to_string_lossy().to_string(),
            signature: "fn get_users() -> String".to_string(),
            error_status: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, &metadata);

        assert!(doc.paths.contains_key("/users"));
        let path_item = doc.paths.get("/users").unwrap();
        assert!(path_item.get.is_some());
        let operation = path_item.get.as_ref().unwrap();
        assert_eq!(operation.operation_id, Some("get_users".to_string()));
    }

    #[test]
    fn test_generate_openapi_with_struct() {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "User".to_string(),
            definition: "struct User { id: i32, name: String }".to_string(),
        });

        let doc = generate_openapi_doc_with_metadata(None, None, &metadata);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
    }

    #[test]
    fn test_generate_openapi_with_route_and_struct() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_content = r#"
use crate::user::User;

pub fn get_user() -> User {
    User { id: 1, name: "Alice".to_string() }
}
"#;
        let route_file = create_temp_file(&temp_dir, "user_route.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "User".to_string(),
            definition: "struct User { id: i32, name: String }".to_string(),
        });
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/user".to_string(),
            function_name: "get_user".to_string(),
            module_path: "test::user_route".to_string(),
            file_path: route_file.to_string_lossy().to_string(),
            signature: "fn get_user() -> User".to_string(),
            error_status: None,
        });

        let doc = generate_openapi_doc_with_metadata(
            Some("Test API".to_string()),
            Some("1.0.0".to_string()),
            &metadata,
        );

        // Check struct schema
        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));

        // Check route
        assert!(doc.paths.contains_key("/user"));
        let path_item = doc.paths.get("/user").unwrap();
        assert!(path_item.get.is_some());
    }

    #[test]
    fn test_generate_openapi_multiple_routes() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        let route1_content = r#"
pub fn get_users() -> String {
    "users".to_string()
}
"#;
        let route1_file = create_temp_file(&temp_dir, "users.rs", route1_content);

        let route2_content = r#"
pub fn create_user() -> String {
    "created".to_string()
}
"#;
        let route2_file = create_temp_file(&temp_dir, "create_user.rs", route2_content);

        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: route1_file.to_string_lossy().to_string(),
            signature: "fn get_users() -> String".to_string(),
            error_status: None,
        });
        metadata.routes.push(RouteMetadata {
            method: "POST".to_string(),
            path: "/users".to_string(),
            function_name: "create_user".to_string(),
            module_path: "test::create_user".to_string(),
            file_path: route2_file.to_string_lossy().to_string(),
            signature: "fn create_user() -> String".to_string(),
            error_status: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, &metadata);

        assert_eq!(doc.paths.len(), 1); // Same path, different methods
        let path_item = doc.paths.get("/users").unwrap();
        assert!(path_item.get.is_some());
        assert!(path_item.post.is_some());
    }

    #[rstest]
    // Test file read failures
    #[case::route_file_read_failure(
        None,
        Some(RouteMetadata {
            method: "GET".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: "/nonexistent/route.rs".to_string(),
            signature: "fn get_users() -> String".to_string(),
            error_status: None,
        }),
        false, // struct should not be added
        false, // route should not be added
    )]
    #[case::route_file_parse_failure(
        None,
        Some(RouteMetadata {
            method: "GET".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: "".to_string(), // Will be set to temp file with invalid syntax
            signature: "fn get_users() -> String".to_string(),
            error_status: None,
        }),
        false, // struct should not be added
        false, // route should not be added
    )]
    fn test_generate_openapi_file_errors(
        #[case] struct_meta: Option<StructMetadata>,
        #[case] route_meta: Option<RouteMetadata>,
        #[case] expect_struct: bool,
        #[case] expect_route: bool,
    ) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mut metadata = CollectedMetadata::new();

        // Handle struct metadata
        if let Some(struct_m) = struct_meta {
            // If file_path is empty, create invalid syntax file
            metadata.structs.push(struct_m);
        }

        // Handle route metadata
        if let Some(mut route_m) = route_meta {
            // If file_path is empty, create invalid syntax file
            if route_m.file_path.is_empty() {
                let invalid_file =
                    create_temp_file(&temp_dir, "invalid_route.rs", "invalid rust syntax {");
                route_m.file_path = invalid_file.to_string_lossy().to_string();
            }
            metadata.routes.push(route_m);
        }

        // Should not panic, just skip invalid files
        let doc = generate_openapi_doc_with_metadata(None, None, &metadata);

        // Check struct
        if expect_struct {
            assert!(doc.components.as_ref().unwrap().schemas.is_some());
            let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
            assert!(schemas.contains_key("User"));
        } else {
            if let Some(schemas) = doc.components.as_ref().unwrap().schemas.as_ref() {
                assert!(!schemas.contains_key("User"));
            }
        }

        // Check route
        if expect_route {
            assert!(doc.paths.contains_key("/users"));
        } else {
            assert!(!doc.paths.contains_key("/users"));
        }

        // Ensure TempDir is properly closed
        drop(temp_dir);
    }
}
