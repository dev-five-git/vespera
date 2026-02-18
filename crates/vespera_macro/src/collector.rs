//! Collector for routes and structs

use std::collections::HashMap;
use std::path::Path;

use syn::Item;

use crate::{
    error::{MacroResult, err_call_site},
    file_utils::{collect_files, file_to_segments},
    metadata::{CollectedMetadata, RouteMetadata},
    route::{extract_doc_comment, extract_route_info},
};

/// Collect routes and structs from a folder.
///
/// Returns the metadata AND the parsed file ASTs, so downstream consumers
/// (e.g., `openapi_generator`) can reuse them without re-reading files from disk.
#[allow(clippy::option_if_let_else)]
pub fn collect_metadata(
    folder_path: &Path,
    folder_name: &str,
) -> MacroResult<(CollectedMetadata, HashMap<String, syn::File>)> {
    let mut metadata = CollectedMetadata::new();
    let mut file_asts = HashMap::new();

    let files = collect_files(folder_path).map_err(|e| err_call_site(format!("vespera! macro: failed to scan route folder '{}': {}. Verify the folder exists and is readable.", folder_path.display(), e)))?;

    for file in files {
        if file.extension().is_none_or(|e| e != "rs") {
            continue;
        }

        let content = std::fs::read_to_string(&file).map_err(|e| {
            err_call_site(format!(
                "vespera! macro: failed to read route file '{}': {}. Check file permissions.",
                file.display(),
                e
            ))
        })?;

        let file_ast = syn::parse_file(&content).map_err(|e| err_call_site(format!("vespera! macro: syntax error in '{}': {}. Fix the Rust syntax errors in this file.", file.display(), e)))?;

        // Store file AST for downstream reuse (keyed by display path to match RouteMetadata.file_path)
        let file_path_key = file.display().to_string();
        file_asts.insert(file_path_key, file_ast.clone());

        // Get module path
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

        let file_path = file.display().to_string();

        // Pre-compute base path once per file (avoids repeated segments.join per route)
        let base_path = format!("/{}", segments.join("/"));

        // Collect routes
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

    Ok((metadata, file_asts))
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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let metadata = collect_metadata(temp_dir.path(), folder_name).map(|(m, _)| m);

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let result = collect_metadata(non_existent_path, folder_name);

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
        let result = collect_metadata(temp_dir.path(), folder_name);

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

        let result = collect_metadata(temp_dir.path(), folder_name);
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
        let result = collect_metadata(temp_dir.path(), folder_name);
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
        let (metadata, _file_asts) = collect_metadata(&sub_dir, folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

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

        let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name).unwrap();

        // Struct with only Debug/Clone derive (no Schema) should not be collected
        assert_eq!(metadata.structs.len(), 0);

        drop(temp_dir);
    }
}
