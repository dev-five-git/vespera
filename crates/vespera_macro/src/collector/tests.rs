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
        // The whole relative path, not just its first segment: the file must
        // be the one that was written (separator-normalised for Windows).
        let normalized = route.file_path.replace('\\', "/");
        assert!(
            normalized.ends_with(*first_filename),
            "route file {normalized} must end with {first_filename}"
        );
    }
}

#[test]
fn test_collect_metadata_without_route_files() {
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
/// Schema structs are no routes; the scan never collects structs (they come
/// from `#[derive(Schema)]` storage), so only the route count is checked.
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
    assert_eq!(route.module_path, "routes");
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
}

#[test]
fn test_collect_metadata_rejects_invalid_syntax() {
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

/// A route file that cannot be read as text (invalid UTF-8 — reproducible
/// on every platform, unlike permission errors) is a compile error naming
/// the file.
#[test]
fn test_collect_metadata_unreadable_route_file_is_an_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";
    let routes = temp_dir.path().join(folder_name);
    fs::create_dir_all(&routes).unwrap();
    fs::write(routes.join("not_utf8.rs"), [0x23, 0xff, 0xfe, 0x0a]).unwrap();

    let error = collect_metadata(temp_dir.path(), folder_name, &[])
        .expect_err("an unreadable route file must be rejected")
        .to_string();
    assert!(error.contains("cannot read or parse"), "{error}");
    assert!(error.contains("not_utf8.rs"), "{error}");
}

#[test]
fn test_collect_metadata_invalid_syntax_is_an_error() {
    // A route file that does not parse is reported, not skipped. (Unreadable
    // files are covered by the `not_utf8.rs` test above.)
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let folder_name = "routes";

    create_temp_file(&temp_dir, "invalid.rs", "{{{");

    let result = collect_metadata(temp_dir.path(), folder_name, &[]);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("syntax error"));
}

#[test]
fn test_collect_metadata_routes_under_the_folder_are_all_collected() {
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
    assert!(metadata.routes.is_empty(), "a plain struct is no route");
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
    assert!(metadata.routes.is_empty(), "a derived struct is no route");
}

#[test]
fn kebab_case_preserves_parameter_underscores() {
    assert_eq!(
        kebab_case_path("/user_groups/{user_id}"),
        "/user-groups/{user_id}"
    );
}

/// Only the file-derived base is kebab-cased; an explicit `path = "..."` is
/// served verbatim.
#[rstest]
#[case::file_only("/user_groups", None, "/user-groups")]
#[case::explicit_kept("/user_groups", Some("/my_route"), "/user-groups/my_route")]
#[case::explicit_param("/", Some("/{item_id}/sub_path"), "/{item_id}/sub_path")]
#[case::explicit_without_slash("/a_b/", Some("c_d"), "/a-b/c_d")]
// `path = "/"` names the file route itself, not a trailing-slash twin.
#[case::explicit_root_in_file("/users", Some("/"), "/users")]
#[case::explicit_empty_in_file("/users", Some(""), "/users")]
#[case::explicit_root_at_root("/", Some("/"), "/")]
fn route_path_kebab_cases_only_the_file_base(
    #[case] base: &str,
    #[case] custom: Option<&str>,
    #[case] expected: &str,
) {
    assert_eq!(build_route_path(base, custom), expected);
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

/// A route module declared behind `#[cfg(..)]` that `#[route]` never
/// registered (compiled out, or not expanded yet when `vespera!` ran) and
/// `#[cfg]`-gated handlers in an ungated file stay out of the documented
/// routes; they are recorded with their predicates so the router mounts them
/// under the same `#[cfg]`.
#[test]
fn cfg_gated_route_modules_and_handlers_are_mounted_conditionally() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "pub mod users;\n#[cfg(feature = \"admin\")]\npub mod admin;\n",
    );
    create_temp_file(
        &temp_dir,
        "users.rs",
        "#[route(get)] pub async fn list() {}\n#[cfg(feature = \"beta\")] #[route(post)] pub async fn beta() {}\n",
    );
    create_temp_file(
        &temp_dir,
        "admin.rs",
        "#[route(get)] pub async fn panel() {}\npub mod stats;\n#[cfg(any())] pub mod broken;\n",
    );
    create_temp_file(
        &temp_dir,
        "admin/stats.rs",
        "#[cfg(unix)] #[route(get, path = \"/{id}\")] pub async fn stats() {}\n",
    );
    // Never read by rustc while the module is compiled out.
    create_temp_file(&temp_dir, "admin/broken.rs", "pub fn broken( {");
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, file_asts) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    let names: Vec<&str> = metadata
        .routes
        .iter()
        .map(|route| route.function_name.as_str())
        .collect();
    assert_eq!(names, ["list"]);
    assert!(!file_asts.keys().any(|file| file.contains("admin")));
    let mut gated: Vec<String> = metadata
        .cfg_routes
        .iter()
        .map(|route| {
            format!(
                "{} {} {}::{} [{}]",
                route.method,
                route.path,
                route.module_path,
                route.function_name,
                route.cfg.join(" & ")
            )
        })
        .collect();
    gated.sort();
    insta::assert_snapshot!("cfg_gated_routes", gated.join("\n"));
}

/// Cfg-disjoint twin `mod` declarations load the module under either
/// predicate, so the route is mounted under their `any` (round-30 M1). Without
/// a `#[route]` storage entry the file takes the slow path, which used to AND
/// the two predicates into `all(unix, not(unix))`: never mounted.
#[test]
fn cfg_twin_module_declarations_mount_the_route_under_either_predicate() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "#[cfg(unix)]\npub mod users;\n#[cfg(not(unix))]\npub mod users;\n",
    );
    create_temp_file(
        &temp_dir,
        "users.rs",
        "#[route(get)] pub async fn list() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    assert!(metadata.routes.is_empty());
    let cfgs: Vec<String> = metadata
        .cfg_routes
        .iter()
        .map(|route| route.cfg.join(" & "))
        .collect();
    assert_eq!(cfgs, ["any(all(unix), all(not (unix)))"]);
}

#[test]
fn inline_module_route_is_a_compile_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "users.rs",
        "mod v2 { #[route(get)] pub async fn list() {} }\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let error = collect_metadata(temp_dir.path(), "routes", &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("inside the inline module `mod v2"),
        "{error}"
    );
}

/// A `cfg(test)` twin and a production fn share the name `list`: the
/// storage entry this build registered decides which one is test-only (by
/// method and signature). A name with a production route in the build is not
/// recorded, the name-keyed record would drop that route too.
#[test]
fn stored_test_only_routes_pick_the_registered_twin() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file = create_temp_file(
        &temp_dir,
        "users.rs",
        "#[cfg(test)]\n#[route(get)]\npub async fn list() {}\n#[route(post)]\npub async fn list() {}\n#[cfg(test)]\n#[route(put)]\npub async fn debug() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();
    let file_path = file.display().to_string();
    let defs = test_scope::fn_definitions(&file);
    let stored = |name: &str, method: &str, signature: &str| StoredRouteInfo {
        fn_name: name.to_string(),
        method: Some(method.to_string()),
        fn_sig_str: signature.to_string(),
        file_path: Some(file_path.clone()),
        ..StoredRouteInfo::default()
    };
    let names = |records: &[crate::metadata::TestOnlyRoute]| -> Vec<String> {
        records.iter().map(|r| r.function_name.clone()).collect()
    };

    // Production build: only the `POST` twin is registered.
    let production = [stored("list", "post", &defs[1].signature)];
    let refs: Vec<&StoredRouteInfo> = production.iter().collect();
    assert_eq!(
        names(&stored_test_only_routes(&file_path, false, &defs, &refs)),
        Vec::<String>::new()
    );

    // Test build: the `GET` twin and `debug` are registered.
    let test_build = [
        stored("list", "get", &defs[0].signature),
        stored("debug", "put", &defs[2].signature),
    ];
    let refs: Vec<&StoredRouteInfo> = test_build.iter().collect();
    assert_eq!(
        names(&stored_test_only_routes(&file_path, false, &defs, &refs)),
        ["list", "debug"]
    );

    // Both twins compiled into one build: only the `GET` twin is test-only,
    // and it carries its method so the `POST` route stays documented.
    let both = [
        stored("list", "get", &defs[0].signature),
        stored("list", "post", &defs[1].signature),
        stored("debug", "put", &defs[2].signature),
    ];
    let refs: Vec<&StoredRouteInfo> = both.iter().collect();
    let recorded = stored_test_only_routes(&file_path, false, &defs, &refs);
    assert_eq!(names(&recorded), ["list", "debug"]);
    assert_eq!(recorded[0].method, "get");
    assert_eq!(recorded[1].method, "put");

    // A file behind a test-only module is test-only as a whole.
    assert_eq!(
        names(&stored_test_only_routes(&file_path, true, &defs, &refs)),
        ["list", "list", "debug"]
    );
}

#[test]
fn stored_inline_module_route_is_a_compile_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file = create_temp_file(
        &temp_dir,
        "users.rs",
        "mod v2 { #[route(get)] pub async fn list() {} }\n",
    );
    crate::schema_macro::file_cache::bump_epoch();
    let storage = [StoredRouteInfo {
        fn_name: "list".to_string(),
        method: Some("get".to_string()),
        custom_path: None,
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        success_status: None,
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
        fn_sig_str: "async fn list()".to_string(),
        inline_module: None,
        file_path: Some(file.display().to_string()),
    }];

    let error = collect_metadata(temp_dir.path(), "routes", &storage).unwrap_err();
    assert!(error.to_string().contains("`list`"), "{error}");
}

/// A nested route folder (`dir = "api/v1"`) maps to a nested module path.
#[test]
fn nested_route_folder_becomes_a_module_path() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "users.rs",
        "#[route(get)] pub async fn list() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();
    let (metadata, _) = collect_metadata(temp_dir.path(), "api/v1", &[]).unwrap();
    assert_eq!(metadata.routes[0].module_path, "api::v1::users");
}

/// `#[route]` recorded the scope: an inline-module handler is rejected from
/// the stored entry alone, and a known top-level one needs no source scan.
#[rstest]
#[case::inline(Some("v2::inner"), true)]
#[case::top_level(Some(""), false)]
fn stored_inline_module_scope_is_authoritative(
    #[case] inline_module: Option<&str>,
    #[case] rejected: bool,
) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    // The source text disagrees with the stored scope on purpose (an IDE
    // buffer ahead of the disk): the stored scope wins.
    let file = create_temp_file(
        &temp_dir,
        "users.rs",
        "mod v2 { #[route(get)] pub async fn list() {} }\n",
    );
    crate::schema_macro::file_cache::bump_epoch();
    let storage = [StoredRouteInfo {
        fn_name: "list".to_string(),
        method: Some("get".to_string()),
        fn_sig_str: "async fn list()".to_string(),
        inline_module: inline_module.map(str::to_string),
        file_path: Some(file.display().to_string()),
        ..StoredRouteInfo::default()
    }];

    let result = collect_metadata(temp_dir.path(), "routes", &storage);
    if rejected {
        let error = result.unwrap_err().to_string();
        // The innermost module, as the source scan reports it: never the
        // `v2::inner` path as a file name (`:` is illegal on Windows).
        assert!(error.contains("`mod inner { .. }`"), "{error}");
        assert!(error.contains("`inner.rs`"), "{error}");
    } else {
        assert_eq!(result.unwrap().0.routes.len(), 1);
    }
}

/// A route file loaded by `#[path]` under another module name: its routes
/// are called through the declared module (`routes::admin`), because the
/// generated call path must resolve; the file's name is only its URL base.
#[test]
fn path_loaded_route_file_is_called_through_its_declared_module() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "#[path = \"admin_impl.rs\"]\npub mod admin;\n",
    );
    create_temp_file(
        &temp_dir,
        "admin_impl.rs",
        "#[route(get)] pub async fn panel() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    let modules: Vec<&str> = metadata
        .routes
        .iter()
        .map(|route| route.module_path.as_str())
        .collect();
    assert_eq!(modules, ["routes::admin"]);
    assert_eq!(metadata.routes[0].path, "/admin-impl");
}

/// A `#[path]` route file in a subdirectory (`beside/support.rs`) behind a
/// `#[cfg]` gate (round-33 R33-M1): its module is `routes::support` (the
/// declared name, not `routes::beside::support`), and the declaration's gate
/// reaches the route, so it is mounted only under that predicate.
#[test]
fn cfg_gated_path_loaded_route_file_in_subdirectory_is_called_through_its_declared_module() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    create_temp_file(
        &temp_dir,
        "mod.rs",
        "#[cfg(feature = \"admin\")]\n#[path = \"beside/support.rs\"]\npub mod support;\n",
    );
    create_temp_file(
        &temp_dir,
        "beside/support.rs",
        "#[route(get)] pub async fn panel() {}\n",
    );
    crate::schema_macro::file_cache::bump_epoch();

    let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &[]).unwrap();

    assert!(metadata.routes.is_empty());
    let gated: Vec<(&str, &str, &[String])> = metadata
        .cfg_routes
        .iter()
        .map(|route| {
            (
                route.module_path.as_str(),
                route.path.as_str(),
                route.cfg.as_slice(),
            )
        })
        .collect();
    assert_eq!(
        gated,
        [(
            "routes::support",
            "/beside/support",
            &["feature = \"admin\"".to_string()][..]
        )]
    );
}
