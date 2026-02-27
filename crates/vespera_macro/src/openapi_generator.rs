//! `OpenAPI` document generator

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use vespera_core::{
    openapi::{Info, OpenApi, OpenApiVersion, Server, Tag},
    route::{HttpMethod, PathItem},
    schema::Components,
};

use crate::{
    file_utils::read_and_parse_file_warn,
    metadata::CollectedMetadata,
    parser::{
        build_operation_from_function, extract_default, extract_field_rename, extract_rename_all,
        parse_enum_to_schema, parse_struct_to_schema, rename_field, strip_raw_prefix_owned,
    },
    schema_macro::type_utils::get_type_default as utils_get_type_default,
};

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
) -> OpenApi {
    let (known_schema_names, struct_definitions) = build_schema_lookups(metadata);
    let file_cache = file_cache.unwrap_or_else(|| build_file_cache(metadata));
    let struct_file_index = build_struct_file_index(&file_cache);
    let parsed_definitions = build_parsed_definitions(metadata);
    let schemas = parse_component_schemas(
        metadata,
        &known_schema_names,
        &struct_definitions,
        &parsed_definitions,
        &file_cache,
        &struct_file_index,
    );
    let (paths, all_tags) = build_path_items(
        metadata,
        &known_schema_names,
        &struct_definitions,
        &file_cache,
    );

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

/// Build schema name and definition lookup maps from metadata.
///
/// Registers ALL structs (including `include_in_openapi: false`) so that
/// `schema_type!` generated types can reference them.
fn build_schema_lookups(
    metadata: &CollectedMetadata,
) -> (HashSet<String>, HashMap<String, String>) {
    let mut known_schema_names = HashSet::with_capacity(metadata.structs.len());
    let mut struct_definitions = HashMap::with_capacity(metadata.structs.len());

    for struct_meta in &metadata.structs {
        struct_definitions.insert(struct_meta.name.clone(), struct_meta.definition.clone());
        known_schema_names.insert(struct_meta.name.clone());
    }

    (known_schema_names, struct_definitions)
}

/// Build file AST cache — parse each unique route file exactly once.
///
/// Deduplicates file paths first, then parses each file a single time.
/// This eliminates redundant file I/O when multiple routes share a source file.
fn build_file_cache(metadata: &CollectedMetadata) -> HashMap<String, syn::File> {
    let unique_paths: BTreeSet<&str> = metadata
        .routes
        .iter()
        .map(|r| r.file_path.as_str())
        .collect();
    let mut cache = HashMap::with_capacity(unique_paths.len());
    for path in unique_paths {
        if let Some(ast) = read_and_parse_file_warn(Path::new(path), "OpenAPI generation") {
            cache.insert(path.to_string(), ast);
        }
    }
    cache
}

/// Build struct name → file path index from cached file ASTs.
///
/// Enables O(1) lookup of which file contains a given struct definition,
/// replacing the previous O(routes × file_read) linear scan.
fn build_struct_file_index(file_cache: &HashMap<String, syn::File>) -> HashMap<String, &str> {
    let mut index = HashMap::with_capacity(file_cache.len() * 4);
    for (path, ast) in file_cache {
        for item in &ast.items {
            if let syn::Item::Struct(s) = item {
                index.insert(s.ident.to_string(), path.as_str());
            }
        }
    }
    index
}

/// Pre-parse all struct/enum definitions into `syn::Item` for reuse.
///
/// Avoids calling `syn::parse_str` per-struct inside `parse_component_schemas()`
/// and other consumers that need the parsed AST.
fn build_parsed_definitions(metadata: &CollectedMetadata) -> HashMap<String, syn::Item> {
    let mut parsed = HashMap::with_capacity(metadata.structs.len());
    for struct_meta in &metadata.structs {
        if let Ok(item) = syn::parse_str::<syn::Item>(&struct_meta.definition) {
            parsed.insert(struct_meta.name.clone(), item);
        }
    }
    parsed
}

/// Parse struct and enum definitions into `OpenAPI` component schemas.
///
/// Only includes structs where `include_in_openapi` is true
/// (i.e., from `#[derive(Schema)]`, not from cross-file lookup).
/// Also processes `#[serde(default)]` attributes to extract default values.
///
/// Uses pre-built `file_cache` and `struct_file_index` for O(1) file lookups
/// instead of scanning all route files per struct.
fn parse_component_schemas(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
    parsed_definitions: &HashMap<String, syn::Item>,
    file_cache: &HashMap<String, syn::File>,
    struct_file_index: &HashMap<String, &str>,
) -> BTreeMap<String, vespera_core::schema::Schema> {
    let mut schemas = BTreeMap::new();

    for struct_meta in metadata.structs.iter().filter(|s| s.include_in_openapi) {
        let Some(parsed) = parsed_definitions.get(&struct_meta.name) else {
            continue;
        };
        let mut schema = match parsed {
            syn::Item::Struct(struct_item) => {
                parse_struct_to_schema(struct_item, known_schema_names, struct_definitions)
            }
            syn::Item::Enum(enum_item) => {
                parse_enum_to_schema(enum_item, known_schema_names, struct_definitions)
            }
            _ => continue,
        };

        // Process default values using cached file ASTs (O(1) lookup)
        if let syn::Item::Struct(struct_item) = parsed {
            let file_ast = struct_file_index
                .get(&struct_meta.name)
                .and_then(|path| file_cache.get(*path))
                .or_else(|| {
                    metadata
                        .routes
                        .first()
                        .and_then(|r| file_cache.get(&r.file_path))
                });

            if let Some(ast) = file_ast {
                process_default_functions(struct_item, ast, &mut schema);
            }
        }

        schemas.insert(struct_meta.name.clone(), schema);
    }

    schemas
}

/// Build path items and collect tags from route metadata.
///
/// Uses pre-built `file_cache` to avoid re-reading and re-parsing source files.
/// Each unique file is parsed exactly once in `build_file_cache`.
fn build_path_items(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
    file_cache: &HashMap<String, syn::File>,
) -> (BTreeMap<String, PathItem>, BTreeSet<String>) {
    let mut paths = BTreeMap::new();
    let mut all_tags = BTreeSet::new();

    // Pre-build function name index for O(1) lookup instead of O(items) per route
    let fn_index: HashMap<&str, HashMap<String, &syn::ItemFn>> = file_cache
        .iter()
        .map(|(path, ast)| {
            let fns: HashMap<String, &syn::ItemFn> = ast
                .items
                .iter()
                .filter_map(|item| {
                    if let syn::Item::Fn(fn_item) = item {
                        Some((fn_item.sig.ident.to_string(), fn_item))
                    } else {
                        None
                    }
                })
                .collect();
            (path.as_str(), fns)
        })
        .collect();

    for route_meta in &metadata.routes {
        let Some(fns) = fn_index.get(route_meta.file_path.as_str()) else {
            continue;
        };

        let Some(fn_item) = fns.get(&route_meta.function_name) else {
            continue;
        };

        let Ok(method) = HttpMethod::try_from(route_meta.method.as_str()) else {
            eprintln!(
                "vespera: skipping route '{}' — unknown HTTP method '{}'",
                route_meta.path, route_meta.method
            );
            continue;
        };

        if let Some(tags) = &route_meta.tags {
            for tag in tags {
                all_tags.insert(tag.clone());
            }
        }

        let mut operation = build_operation_from_function(
            &fn_item.sig,
            &route_meta.path,
            known_schema_names,
            struct_definitions,
            route_meta.error_status.as_deref(),
            route_meta.tags.as_deref(),
        );
        operation.description.clone_from(&route_meta.description);

        let path_item = paths
            .entry(route_meta.path.clone())
            .or_insert_with(PathItem::default);

        path_item.set_operation(method, operation);
    }

    (paths, all_tags)
}

/// Set the default value on an inline property schema, if not already set.
///
/// Looks up `field_name` in the properties map. If found as an inline schema
/// and the schema has no existing default, sets `value` as the default.
fn set_property_default(
    properties: &mut BTreeMap<String, vespera_core::schema::SchemaRef>,
    field_name: &str,
    value: serde_json::Value,
) {
    use vespera_core::schema::SchemaRef;

    if let Some(SchemaRef::Inline(prop_schema)) = properties.get_mut(field_name)
        && prop_schema.default.is_none()
    {
        prop_schema.default = Some(value);
    }
}

/// Process default functions for struct fields
/// This function extracts default values from:
/// 1. `#[schema(default = "value")]` attributes (generated by `schema_type!` from `sea_orm(default_value)`)
/// 2. `#[serde(default = "function_name")]` by finding the function in the file AST
/// 3. `#[serde(default)]` by using type-specific defaults
fn process_default_functions(
    struct_item: &syn::ItemStruct,
    file_ast: &syn::File,
    schema: &mut vespera_core::schema::Schema,
) {
    use syn::Fields;

    // Extract rename_all from struct level
    let struct_rename_all = extract_rename_all(&struct_item.attrs);

    // Get properties from schema
    let Some(properties) = &mut schema.properties else {
        return;
    };

    // Process each field in the struct
    if let Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            let rust_field_name = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |i| strip_raw_prefix_owned(i.to_string()),
            );
            let field_name = extract_field_rename(&field.attrs)
                .unwrap_or_else(|| rename_field(&rust_field_name, struct_rename_all.as_deref()));

            // Priority 1: #[schema(default = "value")] from schema_type! macro
            if let Some(default_str) = extract_schema_default_attr(&field.attrs) {
                let value = parse_default_string_to_json_value(&default_str);
                set_property_default(properties, &field_name, value);
                continue;
            }

            // Priority 2: #[serde(default)] / #[serde(default = "fn")]
            let default_info = match extract_default(&field.attrs) {
                Some(Some(func_name)) => func_name, // default = "function_name"
                Some(None) => {
                    // Simple default (no function) - we can set type-specific defaults
                    if let Some(default_value) = utils_get_type_default(&field.ty) {
                        set_property_default(properties, &field_name, default_value);
                    }
                    continue;
                }
                None => continue, // No default attribute
            };

            // Find the function in the file AST and extract default value
            if let Some(func_item) = find_function_in_file(file_ast, &default_info)
                && let Some(default_value) = extract_default_value_from_function(func_item)
            {
                set_property_default(properties, &field_name, default_value);
            }
        }
    }
}

/// Extract `default` value from `#[schema(default = "...")]` field attribute.
///
/// This attribute is generated by `schema_type!` when converting `sea_orm(default_value)`.
/// It carries the raw default value string for OpenAPI schema generation.
fn extract_schema_default_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("schema"))
        .find_map(|attr| {
            let mut default_value = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    default_value = Some(lit.value());
                }
                Ok(())
            });
            default_value
        })
}

/// Parse a default value string into the appropriate `serde_json::Value`.
///
/// Tries to infer the JSON type: integer → number → bool → string (fallback).
fn parse_default_string_to_json_value(value: &str) -> serde_json::Value {
    // Try integer first
    if let Ok(n) = value.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    // Try float
    if let Ok(f) = value.parse::<f64>()
        && let Some(n) = serde_json::Number::from_f64(f)
    {
        return serde_json::Value::Number(n);
    }
    // Try bool
    if let Ok(b) = value.parse::<bool>() {
        return serde_json::Value::Bool(b);
    }
    // Fallback to string
    serde_json::Value::String(value.to_string())
}

/// Find a function by name in the file AST
fn find_function_in_file<'a>(
    file_ast: &'a syn::File,
    function_name: &str,
) -> Option<&'a syn::ItemFn> {
    file_ast.items.iter().find_map(|item| match item {
        syn::Item::Fn(fn_item) if fn_item.sig.ident == function_name => Some(fn_item),
        _ => None,
    })
}

/// Extract default value from function body
/// This tries to extract literal values from common patterns like:
/// - "`value".to_string()` -> "value"
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
            Lit::Int(i) => i
                .base10_parse::<i64>()
                .ok()
                .map(|v| serde_json::Value::Number(v.into())),
            Lit::Float(f) => f
                .base10_parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number),
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;
    use crate::metadata::{CollectedMetadata, RouteMetadata, StructMetadata};

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.path().join(filename);
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    #[test]
    fn test_generate_openapi_empty_metadata() {
        let metadata = CollectedMetadata::new();

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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

        let doc = generate_openapi_doc_with_metadata(title, version, None, &metadata, None);

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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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
            ..Default::default()
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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
            ..Default::default()
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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
            ..Default::default()
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Message"));
    }

    #[test]
    fn test_generate_openapi_with_enum_and_route() {
        // Test enum used in route to ensure enum parsing is called in route context
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_content = r"
pub fn get_status() -> Status {
    Status::Active
}
";
        let route_file = create_temp_file(&temp_dir, "status_route.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Status".to_string(),
            definition: "enum Status { Active, Inactive }".to_string(),
            ..Default::default()
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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        // Check enum schema
        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Status"));

        // Check route
        assert!(doc.paths.contains_key("/status"));
    }

    #[test]
    fn test_generate_openapi_with_fallback_item() {
        // Test fallback case for non-struct, non-enum items
        // Use a const item which will be parsed as syn::Item::Const first
        // This triggers the fallback case (_ branch) which now gracefully skips
        // items that cannot be parsed as structs (defensive error handling)
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Config".to_string(),
            // This will be parsed as syn::Item::Const, triggering the fallback case
            // which now safely skips this item instead of panicking
            definition: "const CONFIG: i32 = 42;".to_string(),
            include_in_openapi: true,
        });

        // This should gracefully handle the invalid item (skip it) instead of panicking
        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);
        // The invalid struct definition should be skipped, resulting in no schemas
        assert!(doc.components.is_none() || doc.components.as_ref().unwrap().schemas.is_none());
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
            ..Default::default()
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
            None,
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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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
            file_path: String::new(), // Will be set to temp file with invalid syntax
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
        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

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

        let doc = generate_openapi_doc_with_metadata(None, None, Some(servers), &metadata, None);

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
        let value = utils_get_type_default(&ty);
        assert_eq!(value, Some(serde_json::Value::String(String::new())));
    }

    #[test]
    fn test_get_type_default_integers() {
        for type_name in &["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"] {
            let ty: syn::Type = syn::parse_str(type_name).unwrap();
            let value = utils_get_type_default(&ty);
            assert_eq!(
                value,
                Some(serde_json::Value::Number(0.into())),
                "Failed for type {type_name}"
            );
        }
    }

    #[test]
    fn test_get_type_default_floats() {
        for type_name in &["f32", "f64"] {
            let ty: syn::Type = syn::parse_str(type_name).unwrap();
            let value = utils_get_type_default(&ty);
            assert!(value.is_some(), "Failed for type {type_name}");
        }
    }

    #[test]
    fn test_get_type_default_bool() {
        let ty: syn::Type = syn::parse_str("bool").unwrap();
        let value = utils_get_type_default(&ty);
        assert_eq!(value, Some(serde_json::Value::Bool(false)));
    }

    #[test]
    fn test_get_type_default_unknown() {
        let ty: syn::Type = syn::parse_str("CustomType").unwrap();
        let value = utils_get_type_default(&ty);
        assert!(value.is_none());
    }

    #[test]
    fn test_get_type_default_non_path() {
        // Reference type is not a path type
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let value = utils_get_type_default(&ty);
        assert!(value.is_none());
    }

    #[test]
    fn test_find_function_in_file() {
        let file_content = r"
fn foo() {}
fn bar() -> i32 { 42 }
fn baz(x: i32) -> i32 { x }
";
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
            r"
            fn default_value() -> i32 {
                42
            }
        ",
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
            r"
            fn default_value() {
                let x = 1;
            }
        ",
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
            ..Default::default()
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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        // Struct should be present
        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
    }

    #[test]
    fn test_generate_openapi_with_simple_default() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        let route_content = r"
struct Config {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    count: i32,
}

pub fn get_config() -> Config {
    Config { enabled: true, count: 0 }
}
";
        let route_file = create_temp_file(&temp_dir, "config.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Config".to_string(),
            definition:
                r"struct Config { #[serde(default)] enabled: bool, #[serde(default)] count: i32 }"
                    .to_string(),
            ..Default::default()
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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Config"));
    }

    // ======== Tests for uncovered lines ========

    #[test]
    fn test_fallback_struct_finding_in_route_files() {
        // Test line 65: fallback loop that finds struct in any route file when direct search fails
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create TWO route files - struct is in second file, route references it from first
        let route1_content = r"
pub fn get_users() -> Vec<User> {
    vec![]
}
";
        let route1_file = create_temp_file(&temp_dir, "users.rs", route1_content);

        let route2_content = r#"
fn default_name() -> String {
    "Guest".to_string()
}

struct User {
    #[serde(default = "default_name")]
    name: String,
}

pub fn get_user() -> User {
    User { name: "Alice".to_string() }
}
"#;
        let route2_file = create_temp_file(&temp_dir, "user.rs", route2_content);

        let mut metadata = CollectedMetadata::new();
        // Add struct but point to route1 (which doesn't contain the struct)
        // This forces the fallback loop to search other route files
        metadata.structs.push(StructMetadata {
            name: "User".to_string(),
            definition: r#"struct User { #[serde(default = "default_name")] name: String }"#
                .to_string(),
            ..Default::default()
        });
        // Add BOTH routes - the first doesn't contain User struct, so fallback searches the second
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: route1_file.to_string_lossy().to_string(),
            signature: "fn get_users() -> Vec<User>".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });
        metadata.routes.push(RouteMetadata {
            method: "GET".to_string(),
            path: "/user".to_string(),
            function_name: "get_user".to_string(),
            module_path: "test::user".to_string(),
            file_path: route2_file.to_string_lossy().to_string(),
            signature: "fn get_user() -> User".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        // Struct should be found via fallback and processed
        assert!(doc.components.as_ref().unwrap().schemas.is_some());
        let schemas = doc.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
    }

    #[test]
    fn test_process_default_functions_with_no_properties() {
        // Test line 152: early return when schema.properties is None
        // This happens when a struct has no named fields (unit struct or tuple struct)
        use vespera_core::schema::Schema;

        let struct_item: syn::ItemStruct = syn::parse_str("struct Empty;").unwrap();
        let file_ast: syn::File = syn::parse_str("fn foo() {}").unwrap();
        let mut schema = Schema::object();
        schema.properties = None; // Explicitly set to None

        // This should return early without panic
        process_default_functions(&struct_item, &file_ast, &mut schema);

        // Schema should remain unchanged
        assert!(schema.properties.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_int_parse_failure() {
        // Test line 253: int parse failure (overflow)
        // Create an integer literal that's too large to parse as i64
        // Use a literal that syn will parse but i64::parse will fail on
        let expr: syn::Expr = syn::parse_str("999999999999999999999999999999").unwrap();
        let value = extract_value_from_expr(&expr);
        assert!(value.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_float_parse_failure() {
        // Test line 260: float parse failure
        // Create a float literal that's too large/invalid
        let expr: syn::Expr = syn::parse_str("1e999999").unwrap();
        let value = extract_value_from_expr(&expr);
        // This may parse successfully to infinity or fail - either way should handle it
        // The important thing is no panic
        let _ = value;
    }

    #[test]
    fn test_extract_value_from_expr_method_call_with_nested_receiver() {
        // Test lines 275-276: recursive extraction from method call receiver
        // When receiver is not a direct string literal, it tries to extract recursively
        // But the recursive call also won't find a Lit, so it returns None
        // This test verifies the recursive path is exercised (line 275-276)
        let expr: syn::Expr = syn::parse_str(r#"("hello").to_string()"#).unwrap();
        let value = extract_value_from_expr(&expr);
        // The receiver is a Paren expression - recursive call is made but returns None
        // because Paren is not handled in the match
        assert!(value.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_method_call_with_non_literal_receiver() {
        // Test lines 275-276: recursive extraction fails for non-literal
        let expr: syn::Expr = syn::parse_str(r"some_var.to_string()").unwrap();
        let value = extract_value_from_expr(&expr);
        // Cannot extract value from a variable
        assert!(value.is_none());
    }

    #[test]
    fn test_extract_value_from_expr_method_call_chained_to_string() {
        // Test lines 275-276: another case where recursive extraction is attempted
        // Chained method calls: 42.to_string() has int literal as receiver
        let expr: syn::Expr = syn::parse_str(r"42.to_string()").unwrap();
        let value = extract_value_from_expr(&expr);
        // Line 275 recursive call extracts 42 as Number, then line 276 returns it
        assert_eq!(value, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn test_get_type_default_empty_path_segments() {
        // Test empty path segments returns None
        // Create a type with empty path segments

        // Use parse to create a valid type, then we verify the normal path works
        let ty: syn::Type = syn::parse_str("::String").unwrap();
        // This has segments, so it should work
        let value = utils_get_type_default(&ty);
        // Global path ::String still has "String" as last segment
        assert!(value.is_some());

        // Test reference type (non-path type)
        let ref_ty: syn::Type = syn::parse_str("&str").unwrap();
        let ref_value = utils_get_type_default(&ref_ty);
        // Reference is not a Path type, so returns None
        assert!(ref_value.is_none());
    }

    #[test]
    fn test_get_type_default_tuple_type() {
        // Test non-Path type returns None
        let ty: syn::Type = syn::parse_str("(i32, String)").unwrap();
        let value = utils_get_type_default(&ty);
        assert!(value.is_none());
    }

    #[test]
    fn test_get_type_default_array_type() {
        // Test array type returns None
        let ty: syn::Type = syn::parse_str("[i32; 3]").unwrap();
        let value = utils_get_type_default(&ty);
        assert!(value.is_none());
    }

    #[test]
    fn test_build_path_items_unknown_http_method() {
        // Test lines 131-134: route with unknown HTTP method is skipped
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        let route_content = r#"
pub fn get_users() -> String {
    "users".to_string()
}
"#;
        let route_file = create_temp_file(&temp_dir, "users.rs", route_content);

        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "INVALID".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: route_file.to_string_lossy().to_string(),
            signature: "fn get_users() -> String".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        // Route with unknown HTTP method should be skipped entirely
        assert!(
            doc.paths.is_empty(),
            "Route with unknown HTTP method should be skipped"
        );
    }

    #[test]
    fn test_build_path_items_unknown_method_skipped_valid_kept() {
        // Test that unknown methods are skipped while valid routes are kept
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        let route_content = r#"
pub fn get_users() -> String {
    "users".to_string()
}

pub fn create_users() -> String {
    "created".to_string()
}
"#;
        let route_file = create_temp_file(&temp_dir, "users.rs", route_content);
        let file_path = route_file.to_string_lossy().to_string();

        let mut metadata = CollectedMetadata::new();
        // Invalid method route
        metadata.routes.push(RouteMetadata {
            method: "CONNECT".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "test::users".to_string(),
            file_path: file_path.clone(),
            signature: "fn get_users() -> String".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });
        // Valid method route
        metadata.routes.push(RouteMetadata {
            method: "POST".to_string(),
            path: "/users".to_string(),
            function_name: "create_users".to_string(),
            module_path: "test::users".to_string(),
            file_path,
            signature: "fn create_users() -> String".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);

        // Only the valid POST route should appear
        assert_eq!(doc.paths.len(), 1);
        let path_item = doc.paths.get("/users").unwrap();
        assert!(
            path_item.post.is_some(),
            "Valid POST route should be present"
        );
        assert!(
            path_item.get.is_none(),
            "Invalid method route should be skipped"
        );
    }

    #[test]
    fn test_generate_openapi_with_unparseable_definition() {
        // Test line 42: syn::parse_str fails with invalid Rust syntax
        // This triggers the `continue` branch when parsing fails
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Invalid".to_string(),
            // Invalid Rust syntax - cannot be parsed by syn
            definition: "struct { invalid syntax {{{{".to_string(),
            include_in_openapi: true,
        });

        // Should gracefully skip unparseable definitions
        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);
        // The unparseable definition should be skipped
        assert!(doc.components.is_none() || doc.components.as_ref().unwrap().schemas.is_none());
    }

    // ======== Tests for set_property_default helper ========

    #[test]
    fn test_set_property_default_on_inline_schema() {
        use vespera_core::schema::{Schema, SchemaRef};

        let mut properties = BTreeMap::new();
        let mut schema = Schema::object();
        schema.default = None;
        properties.insert("name".to_string(), SchemaRef::Inline(Box::new(schema)));

        set_property_default(
            &mut properties,
            "name",
            serde_json::Value::String("Alice".to_string()),
        );

        if let Some(SchemaRef::Inline(prop)) = properties.get("name") {
            assert_eq!(
                prop.default,
                Some(serde_json::Value::String("Alice".to_string()))
            );
        } else {
            panic!("Expected Inline schema");
        }
    }

    #[test]
    fn test_set_property_default_does_not_overwrite_existing() {
        use vespera_core::schema::{Schema, SchemaRef};

        let mut properties = BTreeMap::new();
        let mut schema = Schema::object();
        schema.default = Some(serde_json::Value::String("existing".to_string()));
        properties.insert("name".to_string(), SchemaRef::Inline(Box::new(schema)));

        set_property_default(
            &mut properties,
            "name",
            serde_json::Value::String("new".to_string()),
        );

        if let Some(SchemaRef::Inline(prop)) = properties.get("name") {
            assert_eq!(
                prop.default,
                Some(serde_json::Value::String("existing".to_string())),
                "Should NOT overwrite existing default"
            );
        } else {
            panic!("Expected Inline schema");
        }
    }

    #[test]
    fn test_set_property_default_skips_ref_schema() {
        use vespera_core::schema::{Reference, SchemaRef};

        let mut properties = BTreeMap::new();
        properties.insert(
            "user".to_string(),
            SchemaRef::Ref(Reference::schema("User")),
        );

        // Should silently no-op (Ref variants have no default field)
        set_property_default(
            &mut properties,
            "user",
            serde_json::Value::String("ignored".to_string()),
        );

        assert!(
            matches!(properties.get("user"), Some(SchemaRef::Ref(_))),
            "Should remain a Ref variant"
        );
    }

    #[test]
    fn test_set_property_default_skips_missing_property() {
        let mut properties = BTreeMap::new();

        // Should silently no-op (property doesn't exist)
        set_property_default(
            &mut properties,
            "nonexistent",
            serde_json::Value::Number(42.into()),
        );

        assert!(properties.is_empty(), "Should not insert new properties");
    }

    #[test]
    fn test_extract_schema_default_attr_with_value() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[schema(default = "42")])];
        let result = extract_schema_default_attr(&attrs);
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn test_extract_schema_default_attr_no_default() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[schema(rename = "foo")])];
        let result = extract_schema_default_attr(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_default_attr_non_schema() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
        let result = extract_schema_default_attr(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_default_string_to_json_value_integer() {
        let result = parse_default_string_to_json_value("42");
        assert_eq!(result, serde_json::Value::Number(42.into()));
    }

    #[test]
    fn test_parse_default_string_to_json_value_float() {
        let result = parse_default_string_to_json_value("2.72");
        assert_eq!(result, serde_json::json!(2.72));
    }

    #[test]
    fn test_parse_default_string_to_json_value_bool() {
        let result = parse_default_string_to_json_value("true");
        assert_eq!(result, serde_json::Value::Bool(true));
    }

    #[test]
    fn test_parse_default_string_to_json_value_string_fallback() {
        let result = parse_default_string_to_json_value("hello world");
        assert_eq!(result, serde_json::Value::String("hello world".to_string()));
    }

    #[test]
    fn test_process_default_functions_with_schema_default_attr() {
        use vespera_core::schema::{Schema, SchemaRef};

        let file_ast: syn::File = syn::parse_str("").unwrap();
        let struct_item: syn::ItemStruct =
            syn::parse_str(r#"pub struct Test { #[schema(default = "100")] pub count: i32 }"#)
                .unwrap();
        let mut schema = Schema::object();
        let props = schema.properties.get_or_insert_with(BTreeMap::new);
        props.insert(
            "count".to_string(),
            SchemaRef::Inline(Box::new(Schema::integer())),
        );
        process_default_functions(&struct_item, &file_ast, &mut schema);
        if let Some(SchemaRef::Inline(prop_schema)) =
            schema.properties.as_ref().unwrap().get("count")
        {
            assert_eq!(prop_schema.default, Some(serde_json::json!(100)));
        } else {
            panic!("Expected inline schema with default");
        }
    }

    #[test]
    fn test_generate_openapi_route_function_not_in_ast() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_content = "pub fn get_items() -> String { \"items\".to_string() }\n";
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

        let doc = generate_openapi_doc_with_metadata(None, None, None, &metadata, None);
        assert!(
            doc.paths.is_empty(),
            "Route with non-matching function should be skipped"
        );
    }
}
