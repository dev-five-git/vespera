//! OpenAPI document generator

use std::collections::{BTreeMap, BTreeSet};
use vespera_core::{
    openapi::{Info, OpenApi, OpenApiVersion, Server, Tag},
    route::{HttpMethod, PathItem},
    schema::Components,
};

use crate::metadata::CollectedMetadata;
use crate::parser::{
    build_operation_from_function, extract_default, extract_field_rename, extract_rename_all,
    parse_enum_to_schema, parse_struct_to_schema, rename_field, strip_raw_prefix,
};

/// Generate OpenAPI document from collected metadata
pub fn generate_openapi_doc_with_metadata(
    title: Option<String>,
    version: Option<String>,
    servers: Option<Vec<Server>>,
    metadata: &CollectedMetadata,
) -> OpenApi {
    let mut paths: BTreeMap<String, PathItem> = BTreeMap::new();
    let mut schemas: BTreeMap<String, vespera_core::schema::Schema> = BTreeMap::new();
    let mut known_schema_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut struct_definitions: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut all_tags: BTreeSet<String> = BTreeSet::new();

    // First, register all schema names and store struct definitions
    for struct_meta in &metadata.structs {
        let schema_name = struct_meta.name.clone();
        known_schema_names.insert(schema_name.clone(), schema_name);
        struct_definitions.insert(struct_meta.name.clone(), struct_meta.definition.clone());
    }

    // Then, parse all struct and enum schemas (now they can reference each other)
    for struct_meta in &metadata.structs {
        let parsed = syn::parse_str::<syn::Item>(&struct_meta.definition).unwrap();
        let mut schema = match &parsed {
            syn::Item::Struct(struct_item) => {
                parse_struct_to_schema(struct_item, &known_schema_names, &struct_definitions)
            }
            syn::Item::Enum(enum_item) => {
                parse_enum_to_schema(enum_item, &known_schema_names, &struct_definitions)
            }
            _ => {
                // Fallback to struct parsing for backward compatibility
                parse_struct_to_schema(
                    &syn::parse_str(&struct_meta.definition).unwrap(),
                    &known_schema_names,
                    &struct_definitions,
                )
            }
        };

        // Process default values for struct fields
        if let syn::Item::Struct(struct_item) = &parsed {
            // Find the file where this struct is defined
            // Try to find a route file that contains this struct
            let struct_file = metadata
                .routes
                .iter()
                .find_map(|route| {
                    // Check if the file contains the struct definition
                    if let Ok(file_content) = std::fs::read_to_string(&route.file_path) {
                        // Check if the struct name appears in the file (more specific check)
                        // Look for "struct StructName" pattern
                        let struct_pattern = format!("struct {}", struct_meta.name);
                        if file_content.contains(&struct_pattern) {
                            return Some(route.file_path.clone());
                        }
                    }
                    None
                })
                .or_else(|| {
                    // Fallback: try all route files to find the struct
                    for route in &metadata.routes {
                        if let Ok(file_content) = std::fs::read_to_string(&route.file_path) {
                            let struct_pattern = format!("struct {}", struct_meta.name);
                            if file_content.contains(&struct_pattern) {
                                return Some(route.file_path.clone());
                            }
                        }
                    }
                    // Last resort: use first route file if available
                    metadata.routes.first().map(|r| r.file_path.clone())
                });

            if let Some(file_path) = struct_file
                && let Ok(file_content) = std::fs::read_to_string(&file_path)
                && let Ok(file_ast) = syn::parse_file(&file_content)
            {
                // Process default functions for struct fields
                process_default_functions(struct_item, &file_ast, &mut schema);
            }
        }

        let schema_name = struct_meta.name.clone();
        schemas.insert(schema_name.clone(), schema);
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

                // Collect tags for global tags list
                if let Some(tags) = &route_meta.tags {
                    for tag in tags {
                        all_tags.insert(tag.clone());
                    }
                }

                // Build operation from function signature
                let mut operation = build_operation_from_function(
                    &fn_item.sig,
                    &route_meta.path,
                    &known_schema_names,
                    &struct_definitions,
                    route_meta.error_status.as_deref(),
                    route_meta.tags.as_deref(),
                );

                // Set description from metadata
                if let Some(desc) = &route_meta.description {
                    operation.description = Some(desc.clone());
                }

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

/// Process default functions for struct fields
/// This function extracts default values from functions specified in #[serde(default = "function_name")]
fn process_default_functions(
    struct_item: &syn::ItemStruct,
    file_ast: &syn::File,
    schema: &mut vespera_core::schema::Schema,
) {
    use syn::Fields;
    use vespera_core::schema::SchemaRef;

    // Extract rename_all from struct level
    let struct_rename_all = extract_rename_all(&struct_item.attrs);

    // Get properties from schema
    let properties = match &mut schema.properties {
        Some(props) => props,
        None => return, // No properties to process
    };

    // Process each field in the struct
    if let Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            // Extract default function name
            let default_info = match extract_default(&field.attrs) {
                Some(Some(func_name)) => func_name, // default = "function_name"
                Some(None) => {
                    // Simple default (no function) - we can set type-specific defaults
                    let rust_field_name = field
                        .ident
                        .as_ref()
                        .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                        renamed
                    } else {
                        rename_field(&rust_field_name, struct_rename_all.as_deref())
                    };

                    // Set type-specific default for simple default
                    if let Some(prop_schema_ref) = properties.get_mut(&field_name)
                        && let SchemaRef::Inline(prop_schema) = prop_schema_ref
                        && prop_schema.default.is_none()
                        && let Some(default_value) = get_type_default(&field.ty)
                    {
                        prop_schema.default = Some(default_value);
                    }
                    continue;
                }
                None => continue, // No default attribute
            };

            // Find the function in the file AST
            let func = find_function_in_file(file_ast, &default_info);
            if let Some(func_item) = func {
                // Extract default value from function body
                if let Some(default_value) = extract_default_value_from_function(func_item) {
                    // Get the field name (with rename applied)
                    let rust_field_name = field
                        .ident
                        .as_ref()
                        .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                        renamed
                    } else {
                        rename_field(&rust_field_name, struct_rename_all.as_deref())
                    };

                    // Set default value in schema
                    if let Some(prop_schema_ref) = properties.get_mut(&field_name)
                        && let SchemaRef::Inline(prop_schema) = prop_schema_ref
                    {
                        prop_schema.default = Some(default_value);
                    }
                }
            }
        }
    }
}

/// Find a function by name in the file AST
fn find_function_in_file<'a>(
    file_ast: &'a syn::File,
    function_name: &str,
) -> Option<&'a syn::ItemFn> {
    for item in &file_ast.items {
        if let syn::Item::Fn(fn_item) = item
            && fn_item.sig.ident == function_name
        {
            return Some(fn_item);
        }
    }
    None
}

/// Extract default value from function body
/// This tries to extract literal values from common patterns like:
/// - "value".to_string() -> "value"
/// - 42 -> 42
/// - true -> true
/// - vec![] -> []
fn extract_default_value_from_function(func: &syn::ItemFn) -> Option<serde_json::Value> {
    // Try to find return statement or expression
    for stmt in &func.block.stmts {
        if let syn::Stmt::Expr(expr, _) = stmt {
            // Direct expression (like "value".to_string())
            if let Some(value) = extract_value_from_expr(expr) {
                return Some(value);
            }
            // Or return statement
            if let syn::Expr::Return(ret) = expr
                && let Some(expr) = &ret.expr
                && let Some(value) = extract_value_from_expr(expr)
            {
                return Some(value);
            }
        }
    }

    None
}

/// Extract value from expression
fn extract_value_from_expr(expr: &syn::Expr) -> Option<serde_json::Value> {
    use syn::{Expr, ExprLit, ExprMacro, Lit};

    match expr {
        // Literal values
        Expr::Lit(ExprLit { lit, .. }) => match lit {
            Lit::Str(s) => Some(serde_json::Value::String(s.value())),
            Lit::Int(i) => {
                if let Ok(val) = i.base10_parse::<i64>() {
                    Some(serde_json::Value::Number(val.into()))
                } else {
                    None
                }
            }
            Lit::Float(f) => {
                if let Ok(val) = f.base10_parse::<f64>() {
                    Some(serde_json::Value::Number(
                        serde_json::Number::from_f64(val).unwrap_or(serde_json::Number::from(0)),
                    ))
                } else {
                    None
                }
            }
            Lit::Bool(b) => Some(serde_json::Value::Bool(b.value)),
            _ => None,
        },
        // Method calls like "value".to_string()
        Expr::MethodCall(method_call) => {
            if method_call.method == "to_string" {
                // Get the receiver (the string literal)
                // Try direct match first
                if let Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) = method_call.receiver.as_ref()
                {
                    return Some(serde_json::Value::String(s.value()));
                }
                // Try to extract from nested expressions (e.g., if the receiver is wrapped)
                if let Some(value) = extract_value_from_expr(method_call.receiver.as_ref()) {
                    return Some(value);
                }
            }
            None
        }
        // Macro calls like vec![]
        Expr::Macro(ExprMacro { mac, .. }) => {
            if mac.path.is_ident("vec") {
                // Try to parse vec![] as empty array
                return Some(serde_json::Value::Array(vec![]));
            }
            None
        }
        _ => None,
    }
}

/// Get type-specific default value for simple #[serde(default)]
fn get_type_default(ty: &syn::Type) -> Option<serde_json::Value> {
    use syn::Type;
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                match segment.ident.to_string().as_str() {
                    "String" => Some(serde_json::Value::String(String::new())),
                    "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => {
                        Some(serde_json::Value::Number(serde_json::Number::from(0)))
                    }
                    "f32" | "f64" => Some(serde_json::Value::Number(
                        serde_json::Number::from_f64(0.0).unwrap_or(serde_json::Number::from(0)),
                    )),
                    "bool" => Some(serde_json::Value::Bool(false)),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

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

        let doc = generate_openapi_doc_with_metadata(title, version, None, &metadata);

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
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
    }

    #[test]
    fn test_generate_openapi_with_enum() {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Status".to_string(),
            definition: "enum Status { Active, Inactive, Pending }".to_string(),
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Status"));
    }

    #[test]
    fn test_generate_openapi_with_enum_with_data() {
        // Test enum with data (tuple and struct variants) to ensure full coverage
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Message".to_string(),
            definition: "enum Message { Text(String), User { id: i32, name: String } }".to_string(),
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Message"));
    }

    #[test]
    fn test_generate_openapi_with_enum_and_route() {
        // Test enum used in route to ensure enum parsing is called in route context
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_content = r#"
pub fn get_status() -> Status {
    Status::Active
}
"#;
        let route_file = create_temp_file(&temp_dir, "status_route.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Status".to_string(),
            definition: "enum Status { Active, Inactive }".to_string(),
        });
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/status".to_string(),
            function_name: "get_status".to_string(),
            module_path: "test::status_route".to_string(),
            file_path: route_file.to_string_lossy().to_string(),
            signature: "fn get_status() -> Status".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        // Check enum schema
        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Status"));

        // Check route
        assert!(doc.paths.contains_key("/status"));
    }

    #[test]
    #[should_panic(expected = "expected `struct`")]
    fn test_generate_openapi_with_fallback_item() {
        // Test fallback case for non-struct, non-enum items (lines 46-48)
        // Use a const item which will be parsed as syn::Item::Const first
        // This triggers the fallback case (_ branch) which tries to parse as struct
        // The fallback will fail to parse const as struct, causing a panic
        // This test verifies that the fallback path (46-48) is executed
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Config".to_string(),
            // This will be parsed as syn::Item::Const, triggering the fallback case
            definition: "const CONFIG: i32 = 42;".to_string(),
        });

        // This should panic when fallback tries to parse const as struct
        let _doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);
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
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(
            Some("Test API".to_string()),
            Some("1.0.0".to_string()),
            None,
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
            tags: None,
            description: None,
        });
        metadata.routes.push(RouteMetadata {
            method: "POST".to_string(),
            path: "/users".to_string(),
            function_name: "create_user".to_string(),
            module_path: "test::create_user".to_string(),
            file_path: route2_file.to_string_lossy().to_string(),
            signature: "fn create_user() -> String".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

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
            tags: None,
            description: None,
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
            tags: None,
            description: None,
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
        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        // Check struct
        if expect_struct {
            assert!(doc.components.as_ref().unwrap().schemas.is_some());
            let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
            assert!(schemas.contains_key("User"));
        } else if let Some(schemas) = doc.components.as_ref().unwrap().schemas.as_ref() {
            assert!(!schemas.contains_key("User"));
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

    #[test]
    fn test_generate_openapi_with_tags_and_description() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
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
            error_status: Some(vec![404]),
            tags: Some(vec!["users".to_string(), "admin".to_string()]),
            description: Some("Get all users".to_string()),
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        // Check route has description
        let path_item = doc.paths.get("/users").unwrap();
        let operation = path_item.get.as_ref().unwrap();
        assert_eq!(operation.description, Some("Get all users".to_string()));

        // Check tags are collected
        assert!(doc.tags.is_some());
        let tags = doc.tags.as_ref().unwrap();
        assert!(tags.iter().any(|t| t.name == "users"));
        assert!(tags.iter().any(|t| t.name == "admin"));
    }

    #[test]
    fn test_generate_openapi_with_servers() {
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

        let doc = generate_openapi_doc_with_metadata(None, None, Some(servers), &metadata);

        assert!(doc.servers.is_some());
        let doc_servers = doc.servers.unwrap();
        assert_eq!(doc_servers.len(), 2);
        assert_eq!(doc_servers[0].url, "https://api.example.com");
        assert_eq!(doc_servers[1].url, "http://localhost:3000");
    }

    #[test]
    fn test_extract_value_from_expr_int() {
        let expr: syn::Expr = syn::parse_str("42").unwrap();
        let value = extract_value_from_expr(&expr);
        assert_eq!(value, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn test_extract_value_from_expr_float() {
        let expr: syn::Expr = syn::parse_str("12.34").unwrap();
        let value = extract_value_from_expr(&expr);
        assert!(value.is_some());
        if let Some(serde_json::Value::Number(n)) = value {
            assert!((n.as_f64().unwrap() - 12.34).abs() < 0.001);
        }
    }

    #[test]
    fn test_extract_value_from_expr_bool() {
        let expr_true: syn::Expr = syn::parse_str("true").unwrap();
        let expr_false: syn::Expr = syn::parse_str("false").unwrap();
        assert_eq!(
            extract_value_from_expr(&expr_true),
            Some(serde_json::Value::Bool(true))
        );
        assert_eq!(
            extract_value_from_expr(&expr_false),
            Some(serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn test_extract_value_from_expr_string() {
        let expr: syn::Expr = syn::parse_str(r#""hello""#).unwrap();
        let value = extract_value_from_expr(&expr);
        assert_eq!(value, Some(serde_json::Value::String("hello".to_string())));
    }

    #[test]
    fn test_extract_value_from_expr_to_string() {
        let expr: syn::Expr = syn::parse_str(r#""hello".to_string()"#).unwrap();
        let value = extract_value_from_expr(&expr);
        assert_eq!(value, Some(serde_json::Value::String("hello".to_string())));
    }

    #[test]
    fn test_extract_value_from_expr_vec_macro() {
        let expr: syn::Expr = syn::parse_str("vec![]").unwrap();
        let value = extract_value_from_expr(&expr);
        assert_eq!(value, Some(serde_json::Value::Array(vec![])));
    }

    #[test]
    fn test_extract_value_from_expr_unsupported() {
        // Binary expression is not supported
        let expr: syn::Expr = syn::parse_str("1 + 2").unwrap();
        let value = extract_value_from_expr(&expr);
        assert!(value.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_method_call_non_to_string() {
        // Method call that's not to_string()
        let expr: syn::Expr = syn::parse_str(r#""hello".len()"#).unwrap();
        let value = extract_value_from_expr(&expr);
        assert!(value.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_unsupported_literal() {
        // Byte literal is not directly supported
        let expr: syn::Expr = syn::parse_str("b'a'").unwrap();
        let value = extract_value_from_expr(&expr);
        assert!(value.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_non_vec_macro() {
        // Other macros like println! are not supported
        let expr: syn::Expr = syn::parse_str(r#"println!("test")"#).unwrap();
        let value = extract_value_from_expr(&expr);
        assert!(value.is_none());
    }

    #[test]
    fn test_get_type_default_string() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        let value = get_type_default(&ty);
        assert_eq!(value, Some(serde_json::Value::String(String::new())));
    }

    #[test]
    fn test_get_type_default_integers() {
        for type_name in &["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"] {
            let ty: syn::Type = syn::parse_str(type_name).unwrap();
            let value = get_type_default(&ty);
            assert_eq!(
                value,
                Some(serde_json::Value::Number(0.into())),
                "Failed for type {}",
                type_name
            );
        }
    }

    #[test]
    fn test_get_type_default_floats() {
        for type_name in &["f32", "f64"] {
            let ty: syn::Type = syn::parse_str(type_name).unwrap();
            let value = get_type_default(&ty);
            assert!(value.is_some(), "Failed for type {}", type_name);
        }
    }

    #[test]
    fn test_get_type_default_bool() {
        let ty: syn::Type = syn::parse_str("bool").unwrap();
        let value = get_type_default(&ty);
        assert_eq!(value, Some(serde_json::Value::Bool(false)));
    }

    #[test]
    fn test_get_type_default_unknown() {
        let ty: syn::Type = syn::parse_str("CustomType").unwrap();
        let value = get_type_default(&ty);
        assert!(value.is_none());
    }

    #[test]
    fn test_get_type_default_non_path() {
        // Reference type is not a path type
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let value = get_type_default(&ty);
        assert!(value.is_none());
    }

    #[test]
    fn test_find_function_in_file() {
        let file_content = r#"
fn foo() {}
fn bar() -> i32 { 42 }
fn baz(x: i32) -> i32 { x }
"#;
        let file_ast: syn::File = syn::parse_str(file_content).unwrap();

        assert!(find_function_in_file(&file_ast, "foo").is_some());
        assert!(find_function_in_file(&file_ast, "bar").is_some());
        assert!(find_function_in_file(&file_ast, "baz").is_some());
        assert!(find_function_in_file(&file_ast, "nonexistent").is_none());
    }

    #[test]
    fn test_extract_default_value_from_function() {
        // Test direct expression return
        let func: syn::ItemFn = syn::parse_str(
            r#"
            fn default_value() -> i32 {
                42
            }
        "#,
        )
        .unwrap();
        let value = extract_default_value_from_function(&func);
        assert_eq!(value, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn test_extract_default_value_from_function_with_return() {
        // Test explicit return statement
        let func: syn::ItemFn = syn::parse_str(
            r#"
            fn default_value() -> String {
                return "hello".to_string()
            }
        "#,
        )
        .unwrap();
        let value = extract_default_value_from_function(&func);
        assert_eq!(value, Some(serde_json::Value::String("hello".to_string())));
    }

    #[test]
    fn test_extract_default_value_from_function_empty() {
        // Test function with no extractable value
        let func: syn::ItemFn = syn::parse_str(
            r#"
            fn default_value() {
                let x = 1;
            }
        "#,
        )
        .unwrap();
        let value = extract_default_value_from_function(&func);
        assert!(value.is_none());
    }

    #[test]
    fn test_generate_openapi_with_default_functions() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create a file with struct that has default function
        let route_content = r#"
fn default_name() -> String {
    "John".to_string()
}

struct User {
    #[serde(default = "default_name")]
    name: String,
}

pub fn get_user() -> User {
    User { name: "Alice".to_string() }
}
"#;
        let route_file = create_temp_file(&temp_dir, "user.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "User".to_string(),
            definition: r#"struct User { #[serde(default = "default_name")] name: String }"#
                .to_string(),
        });
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/user".to_string(),
            function_name: "get_user".to_string(),
            module_path: "test::user".to_string(),
            file_path: route_file.to_string_lossy().to_string(),
            signature: "fn get_user() -> User".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        // Struct should be present
        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
    }

    #[test]
    fn test_generate_openapi_with_simple_default() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        let route_content = r#"
struct Config {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    count: i32,
}

pub fn get_config() -> Config {
    Config { enabled: true, count: 0 }
}
"#;
        let route_file = create_temp_file(&temp_dir, "config.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Config".to_string(),
            definition:
                r#"struct Config { #[serde(default)] enabled: bool, #[serde(default)] count: i32 }"#
                    .to_string(),
        });
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/config".to_string(),
            function_name: "get_config".to_string(),
            module_path: "test::config".to_string(),
            file_path: route_file.to_string_lossy().to_string(),
            signature: "fn get_config() -> Config".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Config"));
    }
}
