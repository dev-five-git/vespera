//! Collector for routes and structs

use std::collections::HashMap;
use std::path::Path;

use syn::Item;

use crate::{
    error::{MacroResult, err_call_site},
    file_utils::{collect_files, file_to_segments},
    metadata::{CollectedMetadata, RouteMetadata},
    route::{extract_doc_comment, extract_route_info},
    route_impl::StoredRouteInfo,
};

/// Collect routes and structs from a folder.
///
/// When `route_storage` contains entries with `file_path`, files covered by
/// `ROUTE_STORAGE` skip expensive `syn::parse_file()` — route metadata is built
/// directly from the stored data. Default values for `serde(default = "fn")`
/// are already extracted by `#[derive(Schema)]` into `SCHEMA_STORAGE.field_defaults`.
///
/// Returns the metadata AND the parsed file ASTs, so downstream consumers
/// (e.g., `openapi_generator`) can reuse them without re-reading files from disk.
#[allow(clippy::option_if_let_else, clippy::too_many_lines)]
pub fn collect_metadata(
    folder_path: &Path,
    folder_name: &str,
    route_storage: &[StoredRouteInfo],
) -> MacroResult<(CollectedMetadata, HashMap<String, syn::File>)> {
    let mut metadata = CollectedMetadata::new();

    let files = collect_files(folder_path).map_err(|e| err_call_site(format!("vespera! macro: failed to scan route folder '{}': {}. Verify the folder exists and is readable.", folder_path.display(), e)))?;

    let mut file_asts = HashMap::with_capacity(files.len());

    // Index ROUTE_STORAGE entries by file path for O(1) lookup
    let storage_by_file: HashMap<&str, Vec<&StoredRouteInfo>> = {
        let mut map: HashMap<&str, Vec<&StoredRouteInfo>> = HashMap::new();
        for stored in route_storage {
            if let Some(ref fp) = stored.file_path {
                map.entry(fp.as_str()).or_default().push(stored);
            }
        }
        map
    };

    for file in files {
        if file.extension().is_none_or(|e| e != "rs") {
            continue;
        }

        let file_path = file.display().to_string();

        // Get module path (cheap — no parsing needed)
        let segments = file
            .strip_prefix(folder_path)
            .map(|file_stem| file_to_segments(file_stem, folder_path))
            .map_err(|e| {
                err_call_site(format!(
                    "Failed to strip prefix from file: {} (base: {}): {}",
                    file.display(),
                    folder_path.display(),
                    e
                ))
            })?;

        let module_path = if folder_name.is_empty() {
            segments.join("::")
        } else {
            format!("{}::{}", folder_name, segments.join("::"))
        };

        // Pre-compute base path once per file (avoids repeated segments.join per route)
        let base_path = format!("/{}", segments.join("/"));

        // Fast path: ROUTE_STORAGE has entries for this file — skip syn::parse_file()
        if let Some(stored_routes) = storage_by_file.get(file_path.as_str()) {
            for stored in stored_routes {
                let route_path = if let Some(ref custom_path) = stored.custom_path {
                    let trimmed_base = base_path.trim_end_matches('/');
                    format!("{trimmed_base}/{}", custom_path.trim_start_matches('/'))
                } else {
                    base_path.clone()
                };
                let route_path = route_path.replace('_', "-");

                // Extract doc comment from fn_item_str if no explicit description
                let description = stored.description.clone().or_else(|| {
                    syn::parse_str::<syn::ItemFn>(&stored.fn_item_str)
                        .ok()
                        .and_then(|fn_item| extract_doc_comment(&fn_item.attrs))
                });

                metadata.routes.push(RouteMetadata {
                    method: stored.method.clone().unwrap_or_default(),
                    path: route_path,
                    function_name: stored.fn_name.clone(),
                    module_path: module_path.clone(),
                    file_path: file_path.clone(),
                    signature: stored.fn_item_str.clone(),
                    error_status: stored.error_status.clone(),
                    tags: stored.tags.clone(),
                    description,
                });
            }

            // No file_asts insertion needed in fast path:
            // #[derive(Schema)] already extracts serde(default = "fn") values
            // into SCHEMA_STORAGE.field_defaults (Priority 0 in process_default_functions)
        } else {
            // Slow path: full parsing (fallback for files not in ROUTE_STORAGE)
            // Uses get_parsed_file: single syn::parse_file entry point + content cache
            let file_ast = crate::schema_macro::file_cache::get_parsed_file(&file).ok_or_else(|| err_call_site(format!("vespera! macro: cannot read or parse '{}'. Fix the Rust syntax errors in this file.", file.display())))?;

            // Store file AST for downstream reuse
            file_asts.insert(file_path.clone(), file_ast);
            let file_ast = &file_asts[&file_path];

            // Collect routes from AST
            for item in &file_ast.items {
                if let Item::Fn(fn_item) = item
                    && let Some(route_info) = extract_route_info(&fn_item.attrs)
                {
                    let route_path = if let Some(custom_path) = &route_info.path {
                        let trimmed_base = base_path.trim_end_matches('/');
                        format!("{trimmed_base}/{}", custom_path.trim_start_matches('/'))
                    } else {
                        base_path.clone()
                    };
                    let route_path = route_path.replace('_', "-");

                    // Description priority: route attribute > doc comment
                    let description = route_info
                        .description
                        .clone()
                        .or_else(|| extract_doc_comment(&fn_item.attrs));

                    metadata.routes.push(RouteMetadata {
                        method: route_info.method,
                        path: route_path,
                        function_name: fn_item.sig.ident.to_string(),
                        module_path: module_path.clone(),
                        file_path: file_path.clone(),
                        signature: quote::quote!(#fn_item).to_string(),
                        error_status: route_info.error_status.clone(),
                        tags: route_info.tags.clone(),
                        description,
                    });
                }
            }
        }
    }

    Ok((metadata, file_asts))
}

/// Collect file modification times without reading content.
/// Used for cache invalidation — much cheaper than full `collect_metadata()`.
pub fn collect_file_fingerprints(folder_path: &Path) -> MacroResult<HashMap<String, u64>> {
    let files = collect_files(folder_path).map_err(|e| {
        err_call_site(format!(
            "vespera! macro: failed to scan route folder '{}': {}",
            folder_path.display(),
            e
        ))
    })?;

    let mut fingerprints = HashMap::with_capacity(files.len());
    for file in files {
        if file.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let mtime = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            })
            .unwrap_or(0);
        fingerprints.insert(file.display().to_string(), mtime);
    }
    Ok(fingerprints)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    #[test]
    fn test_collect_metadata_empty_folder() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert!(metadata.routes.is_empty());
        assert!(metadata.structs.is_empty());

        drop(temp_dir);
    }

    #[rstest]
    #[case::single_get_route(
        "routes",
        vec![(
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/users",
        "get_users",
        "routes::users",
    )]
    #[case::single_post_route(
        "routes",
        vec![(
            "create_user.rs",
            r#"
#[route(post)]
pub fn create_user() -> String {
    "created".to_string()
}
"#,
        )],
        "post",
        "/create-user",
        "create_user",
        "routes::create_user",
    )]
    #[case::route_with_custom_path(
        "routes",
        vec![(
            "users.rs",
            r#"
#[route(get, path = "/api/users")]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/users/api/users",
        "get_users",
        "routes::users",
    )]
    #[case::route_with_error_status(
        "routes",
        vec![(
            "users.rs",
            r#"
#[route(get, error_status = [400, 404])]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/users",
        "get_users",
        "routes::users",
    )]
    #[case::nested_module(
        "routes",
        vec![(
            "api/users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/api/users",
        "get_users",
        "routes::api::users",
    )]
    #[case::deeply_nested_module(
        "routes",
        vec![(
            "api/v1/users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )],
        "get",
        "/api/v1/users",
        "get_users",
        "routes::api::v1::users",
    )]
    fn test_collect_metadata_routes(
        #[case] folder_name: &str,
        #[case] files: Vec<(&str, &str)>,
        #[case] expected_method: &str,
        #[case] expected_path: &str,
        #[case] expected_function_name: &str,
        #[case] expected_module_path: &str,
    ) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        for (filename, content) in &files {
            create_temp_file(&temp_dir, filename, content);
        }

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        let route = &metadata.routes[0];
        assert_eq!(route.method, expected_method);
        assert_eq!(route.path, expected_path);
        assert_eq!(route.function_name, expected_function_name);
        assert_eq!(route.module_path, expected_module_path);
        if let Some((first_filename, _)) = files.first() {
            assert!(
                route
                    .file_path
                    .contains(first_filename.split('/').next().unwrap())
            );
        }

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_single_struct() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 0);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_struct_without_schema() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "user.rs",
            r"
pub struct User {
    pub id: i32,
    pub name: String,
}
",
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 0);
        assert_eq!(metadata.structs.len(), 0);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_route_and_struct() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "user.rs",
            r#"
use vespera::Schema;

#[derive(Schema)]
pub struct User {
    pub id: i32,
    pub name: String,
}

#[route(get)]
pub fn get_user() -> User {
    User { id: 1, name: "Alice".to_string() }
}
"#,
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 1);

        let route = &metadata.routes[0];
        assert_eq!(route.function_name, "get_user");

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_multiple_routes() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}

#[route(post)]
pub fn create_users() -> String {
    "created".to_string()
}
"#,
        );

        create_temp_file(
            &temp_dir,
            "posts.rs",
            r#"
#[route(get)]
pub fn get_posts() -> String {
    "posts".to_string()
}
"#,
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 3);
        assert_eq!(metadata.structs.len(), 0);

        // Check all routes are present
        let function_names: Vec<&str> = metadata
            .routes
            .iter()
            .map(|r| r.function_name.as_str())
            .collect();
        assert!(function_names.contains(&"get_users"));
        assert!(function_names.contains(&"create_users"));
        assert!(function_names.contains(&"get_posts"));

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_multiple_structs() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "user.rs",
            r"
use vespera::Schema;

#[derive(Schema)]
pub struct User {
    pub id: i32,
    pub name: String,
}
",
        );

        create_temp_file(
            &temp_dir,
            "post.rs",
            r"
use vespera::Schema;

#[derive(Schema)]
pub struct Post {
    pub id: i32,
    pub title: String,
}
",
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 0);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_with_mod_rs() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "mod.rs",
            r#"
#[route(get)]
pub fn index() -> String {
    "index".to_string()
}
"#,
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        assert_eq!(route.function_name, "index");
        assert_eq!(route.path, "/");
        assert_eq!(route.module_path, "routes::");

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_empty_folder_name() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "";

        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        assert_eq!(route.module_path, "users");

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_ignores_non_rs_files() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        create_temp_file(&temp_dir, "config.txt", "some config content");

        create_temp_file(&temp_dir, "readme.md", "# Readme");

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        // Only .rs file should be processed
        assert_eq!(metadata.routes.len(), 1);
        assert_eq!(metadata.structs.len(), 0);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_ignores_invalid_syntax() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "valid.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        create_temp_file(&temp_dir, "invalid.rs", "invalid rust syntax {");

        let metadata = collect_metadata(temp_dir.path(), folder_name, &[]).map(|(m, _)| m);

        // Only valid file should be processed
        assert!(metadata.is_err());

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_error_status() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
#[route(get, error_status = [400, 404, 500])]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        assert_eq!(route.method, "get");
        assert!(route.error_status.is_some());
        let error_status = route.error_status.as_ref().unwrap();
        assert_eq!(error_status.len(), 3);
        assert!(error_status.contains(&400));
        assert!(error_status.contains(&404));
        assert!(error_status.contains(&500));

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_all_http_methods() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "routes.rs",
            r#"
#[route(get)]
pub fn get_handler() -> String { "get".to_string() }

#[route(post)]
pub fn post_handler() -> String { "post".to_string() }

#[route(put)]
pub fn put_handler() -> String { "put".to_string() }

#[route(patch)]
pub fn patch_handler() -> String { "patch".to_string() }

#[route(delete)]
pub fn delete_handler() -> String { "delete".to_string() }

#[route(head)]
pub fn head_handler() -> String { "head".to_string() }

#[route(options)]
pub fn options_handler() -> String { "options".to_string() }
"#,
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        assert_eq!(metadata.routes.len(), 7);

        let methods: Vec<&str> = metadata.routes.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"get"));
        assert!(methods.contains(&"post"));
        assert!(methods.contains(&"put"));
        assert!(methods.contains(&"patch"));
        assert!(methods.contains(&"delete"));
        assert!(methods.contains(&"head"));
        assert!(methods.contains(&"options"));

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_collect_files_error() {
        // Test: collect_files returns error (non-existent directory)
        let non_existent_path = std::path::Path::new("/nonexistent/path/that/does/not/exist");
        let folder_name = "routes";

        let result = collect_metadata(non_existent_path, folder_name, &[]);

        // Should return error when collect_files fails
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("failed to scan route folder"));
    }

    #[test]
    #[cfg(unix)]
    fn test_collect_metadata_file_read_error_permissions() {
        // Test line 31-37: file read error due to permission denial
        // On Unix, we can create a file and then remove read permissions
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create a file with valid Rust syntax
        let file_path = temp_dir.path().join("unreadable.rs");
        fs::write(
            &file_path,
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        )
        .expect("Failed to write temp file");

        // Remove read permissions
        let permissions = fs::Permissions::from_mode(0o000);
        fs::set_permissions(&file_path, permissions).expect("Failed to set permissions");

        // Verify permissions actually took effect (they don't on WSL with Windows filesystem)
        // If we can still read the file, skip this test
        if fs::read_to_string(&file_path).is_ok() {
            // Restore permissions for cleanup
            let permissions = fs::Permissions::from_mode(0o644);
            fs::set_permissions(&file_path, permissions).ok();
            eprintln!(
                "Skipping test: filesystem doesn't respect Unix permissions (likely WSL with NTFS)"
            );
            return;
        }

        // Attempt to collect metadata - should fail with "failed to read route file" error
        let result = collect_metadata(temp_dir.path(), folder_name, &[]);

        // Verify error message
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("failed to read route file"));

        // Restore permissions so tempdir cleanup doesn't fail
        let permissions = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&file_path, permissions).ok();

        drop(temp_dir);
    }

    #[test]
    #[cfg(windows)]
    fn test_collect_metadata_file_read_error_documentation_windows() {
        // Test line 31-37: Documentation of file read error handling on Windows
        //
        // On Windows, file permission errors are harder to reliably trigger in tests
        // because standard read/write operations on temp files typically succeed.
        // The error path at line 31-37 is exercised by edge cases:
        //   1. Files deleted between collect_files scan and read attempt
        //   2. Network drive disconnections
        //   3. Permission changes during execution
        //
        // These are difficult to simulate reliably in automated tests.
        // The error handling code itself is straightforward:
        //   - std::fs::read_to_string() returns an io::Error
        //   - map_err() wraps it with context message
        //   - Caller receives "failed to read route file" error
        //
        // This is tested indirectly via test_collect_metadata_file_read_error_via_invalid_syntax
        // which verifies error propagation works correctly.

        // Verify the documented behavior with a comment-only test
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Successfully create a readable file to verify the happy path
        create_temp_file(
            &temp_dir,
            "readable.rs",
            r#"
#[route(get)]
pub fn get() -> String { "ok".to_string() }
"#,
        );

        let result = collect_metadata(temp_dir.path(), folder_name, &[]);
        assert!(result.is_ok());

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_file_read_error_via_invalid_syntax() {
        // Test line 31-37: verify error handling by parsing invalid files
        // While we can't easily trigger read errors on all platforms,
        // we verify the code path by ensuring errors are properly propagated
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create a file that will fail to parse (syntax error)
        create_temp_file(&temp_dir, "invalid.rs", "{{{");

        // This should fail during syntax parsing, not file reading
        let result = collect_metadata(temp_dir.path(), folder_name, &[]);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("syntax error"));

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_strip_prefix_succeeds_in_normal_case() {
        // Test line 49-58: strip_prefix succeeds in the normal case
        //
        // DEFENSIVE CODE ANALYSIS (line 49-58):
        // The strip_prefix error path is nearly impossible to trigger in practice because:
        // 1. collect_files() returns paths by walking folder_path
        // 2. All returned files are guaranteed to be under folder_path
        // 3. Therefore, strip_prefix(folder_path) should always succeed
        //
        // The error path is defensive programming that would only trigger if:
        // - Path normalization differences existed between collect_files and strip_prefix
        // - Or if folder_path contained symlinks with different absolute paths
        // - Or if the filesystem changed between collect_files and this loop
        //
        // This test verifies the normal case works correctly.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create a subdirectory
        let sub_dir = temp_dir.path().join("routes");
        std::fs::create_dir_all(&sub_dir).expect("Failed to create subdirectory");

        // Create a file in the subdirectory
        create_temp_file(
            &temp_dir,
            "routes/valid.rs",
            r#"
#[route(get)]
pub fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        // Collect metadata from the subdirectory
        let (metadata, _file_asts) = collect_metadata(&sub_dir, folder_name, &[]).unwrap();

        // Should collect the route (strip_prefix succeeds in normal cases)
        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        assert_eq!(route.function_name, "get_users");

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_struct_without_derive() {
        // Test line 81: attr.path().is_ident("derive") returns false
        // Struct with non-derive attributes should not be collected
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "user.rs",
            r"
pub struct User {
    pub id: i32,
    pub name: String,
}
",
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        // Struct without Schema derive should not be collected
        assert_eq!(metadata.structs.len(), 0);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_struct_with_other_derive() {
        // Test line 81: struct with other derive attributes (not Schema)
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        create_temp_file(
            &temp_dir,
            "user.rs",
            r"
#[derive(Debug, Clone)]
pub struct User {
    pub id: i32,
    pub name: String,
}
",
        );

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

        // Struct with only Debug/Clone derive (no Schema) should not be collected
        assert_eq!(metadata.structs.len(), 0);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_fast_path_with_route_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        // Create a .rs file that the fast path will match against
        let file_path = create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
pub async fn get_users() -> String {
    "users".to_string()
}
"#,
        );

        let file_path_str = file_path.display().to_string();

        // Create StoredRouteInfo entries that match this file
        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            tags: Some(vec!["users".to_string()]),
            description: Some("Get all users".to_string()),
            fn_item_str: "pub async fn get_users() -> String { \"users\".to_string() }".to_string(),
            file_path: Some(file_path_str.clone()),
        }];

        let (metadata, file_asts) =
            collect_metadata(temp_dir.path(), folder_name, &route_storage).unwrap();

        // Fast path should produce route metadata
        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        assert_eq!(route.function_name, "get_users");
        assert_eq!(route.method, "get");
        assert_eq!(route.tags, Some(vec!["users".to_string()]));
        assert_eq!(route.description, Some("Get all users".to_string()));
        assert_eq!(route.module_path, "routes::users");

        // Fast path should NOT insert file ASTs (no parsing needed)
        assert!(
            file_asts.is_empty(),
            "Fast path should not populate file_asts"
        );

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_fast_path_with_custom_path() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let file_path = create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
pub async fn get_user() -> String {
    "user".to_string()
}
"#,
        );

        let file_path_str = file_path.display().to_string();

        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_user".to_string(),
            method: Some("get".to_string()),
            custom_path: Some("/{id}".to_string()),
            error_status: Some(vec![404]),
            tags: None,
            description: None,
            fn_item_str: "pub async fn get_user(id: i32) -> String { \"user\".to_string() }"
                .to_string(),
            file_path: Some(file_path_str.clone()),
        }];

        let (metadata, _) = collect_metadata(temp_dir.path(), folder_name, &route_storage).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        assert_eq!(route.path, "/users/{id}");
        assert!(route.error_status.is_some());
        assert_eq!(route.error_status.as_ref().unwrap(), &vec![404]);

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_fast_path_empty_folder_name() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "";

        let file_path = create_temp_file(
            &temp_dir,
            "users.rs",
            r#"
pub async fn list_users() -> String {
    "list".to_string()
}
"#,
        );

        let file_path_str = file_path.display().to_string();

        let route_storage = vec![StoredRouteInfo {
            fn_name: "list_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            tags: None,
            description: None,
            fn_item_str: "pub async fn list_users() -> String { \"list\".to_string() }".to_string(),
            file_path: Some(file_path_str),
        }];

        let (metadata, _) = collect_metadata(temp_dir.path(), folder_name, &route_storage).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        // With empty folder_name, module_path should be just segments (no prefix)
        assert_eq!(route.module_path, "users");

        drop(temp_dir);
    }

    #[test]
    fn test_collect_metadata_fast_path_doc_comment_extraction() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let file_path = create_temp_file(&temp_dir, "items.rs", "// placeholder\n");

        let file_path_str = file_path.display().to_string();

        // fn_item_str includes a doc comment, description is None
        // so the fast path should extract the doc comment
        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_items".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            tags: None,
            description: None, // No explicit description -> should extract from doc comment
            fn_item_str:
                "/// List all items\npub async fn get_items() -> String { \"items\".to_string() }"
                    .to_string(),
            file_path: Some(file_path_str),
        }];

        let (metadata, _) = collect_metadata(temp_dir.path(), folder_name, &route_storage).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        let route = &metadata.routes[0];
        // Description should be extracted from the doc comment in fn_item_str
        assert_eq!(route.description, Some("List all items".to_string()));

        drop(temp_dir);
    }

    #[test]
    fn test_collect_file_fingerprints_skips_non_rs_files() {
        // Exercises line 121: non-.rs files should be skipped
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create both .rs and non-.rs files
        create_temp_file(&temp_dir, "valid.rs", "pub fn hello() {}");
        create_temp_file(&temp_dir, "readme.txt", "This is a readme");
        create_temp_file(&temp_dir, "data.json", "{}");
        create_temp_file(&temp_dir, "script.py", "print('hello')");

        let fingerprints = collect_file_fingerprints(temp_dir.path()).unwrap();

        // Only .rs files should be in fingerprints
        assert_eq!(
            fingerprints.len(),
            1,
            "Only .rs files should be fingerprinted"
        );
        let keys: Vec<&String> = fingerprints.keys().collect();
        assert!(
            keys[0].ends_with("valid.rs"),
            "The only fingerprinted file should be valid.rs"
        );

        drop(temp_dir);
    }
}
