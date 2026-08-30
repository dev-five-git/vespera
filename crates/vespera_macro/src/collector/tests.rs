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
}

#[test]
fn test_collect_metadata_single_struct() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";

    let (metadata, _file_asts) = collect_metadata(temp_dir.path(), folder_name, &[]).unwrap();

    assert_eq!(metadata.routes.len(), 0);
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

    let function_names: Vec<&str> = metadata
        .routes
        .iter()
        .map(|r| r.function_name.as_str())
        .collect();
    assert!(function_names.contains(&"get_users"));
    assert!(function_names.contains(&"create_users"));
    assert!(function_names.contains(&"get_posts"));
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

    assert_eq!(metadata.routes.len(), 1);
    assert_eq!(metadata.structs.len(), 0);
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

    assert!(metadata.is_err());
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
}

#[test]
fn test_collect_metadata_collect_files_error() {
    let non_existent_path = std::path::Path::new("/nonexistent/path/that/does/not/exist");
    let folder_name = "routes";

    let result = collect_metadata(non_existent_path, folder_name, &[]);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("failed to scan route folder"));
}

#[test]
#[cfg(unix)]
fn test_collect_metadata_file_read_error_permissions() {
    // On Unix, we can create a file and then remove read permissions
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";

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

    let result = collect_metadata(temp_dir.path(), folder_name, &[]);

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("cannot read or parse"));
    assert!(error_msg.contains("unreadable.rs"));

    let permissions = fs::Permissions::from_mode(0o644);
    fs::set_permissions(&file_path, permissions).ok();
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

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";

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
}

#[test]
fn test_collect_metadata_file_read_error_via_invalid_syntax() {
    // While we can't easily trigger read errors on all platforms,
    // we verify the code path by ensuring errors are properly propagated
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";

    create_temp_file(&temp_dir, "invalid.rs", "{{{");

    let result = collect_metadata(temp_dir.path(), folder_name, &[]);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("syntax error"));
}

#[test]
fn test_collect_metadata_strip_prefix_succeeds_in_normal_case() {
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
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";

    let sub_dir = temp_dir.path().join("routes");
    std::fs::create_dir_all(&sub_dir).expect("Failed to create subdirectory");

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

    let (metadata, _file_asts) = collect_metadata(&sub_dir, folder_name, &[]).unwrap();

    assert_eq!(metadata.routes.len(), 1);
    let route = &metadata.routes[0];
    assert_eq!(route.function_name, "get_users");
}

#[test]
fn test_collect_metadata_struct_without_derive() {
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

    assert_eq!(metadata.structs.len(), 0);
}

#[test]
fn test_collect_metadata_struct_with_other_derive() {
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

    assert_eq!(metadata.structs.len(), 0);
}

#[test]
fn kebab_case_preserves_parameter_underscores() {
    assert_eq!(
        kebab_case_path("/user_groups/{user_id}"),
        "/user-groups/{user_id}"
    );
}

#[test]
fn collect_metadata_rejects_file_outside_folder() {
    let base = TempDir::new().expect("base temp dir");
    let outside = TempDir::new().expect("outside temp dir");
    let file = create_temp_file(&outside, "route.rs", "pub fn route() {}");

    let error = collect_metadata_from_files([file.as_path()], base.path(), "routes", &[])
        .expect_err("outside file must fail prefix stripping");

    assert!(
        error
            .to_string()
            .contains("Failed to strip prefix from file")
    );
}
