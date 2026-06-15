//! Route-folder scanning and the path-normalization key that makes
//! `#[route]`'s cwd-relative span paths comparable with the
//! collector's absolute walk paths (the fast-path match).

use std::collections::HashMap;
use std::path::Path;

use crate::error::{MacroResult, err_call_site};

/// Normalize a path string into a comparison key **without touching
/// the filesystem** (an earlier `fs::canonicalize` version cost one
/// syscall per lookup — ~130ms for a 300-file project on Windows).
///
/// `#[route]` records `Span::local_file()`, which rustc reports
/// relative to its invocation directory, while the collector walks
/// `{CARGO_MANIFEST_DIR}/src/{folder}` producing absolute paths with
/// platform separators.  This key makes both comparable:
/// - relative paths are absolutized against `cwd` (the same process
///   working directory rustc resolved the span path from)
/// - `.`/`..` components are folded
/// - separators normalize to `/`, the Windows `\\?\` verbatim prefix
///   is stripped, and (Windows only) the drive letter case is folded
pub fn normalize_path_key(path: &str, cwd: &Path) -> String {
    use std::path::Component;

    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let mut folded = std::path::PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    let mut key = folded.display().to_string().replace('\\', "/");
    if let Some(stripped) = key.strip_prefix("//?/") {
        key = stripped.to_owned();
    }
    if cfg!(windows) {
        key.make_ascii_lowercase();
    }
    key
}

/// Single directory walk returning `(path, mtime)` pairs — the shared
/// scan that both cache fingerprinting and route collection consume.
pub fn scan_route_folder(folder_path: &Path) -> MacroResult<Vec<(std::path::PathBuf, u64)>> {
    crate::file_utils::collect_files_with_mtimes(folder_path).map_err(|e| {
        err_call_site(format!(
            "vespera! macro: failed to scan route folder '{}': {}. Verify the folder exists and is readable.",
            folder_path.display(),
            e
        ))
    })
}

/// Build the cache fingerprint map (`.rs` files only) from a scan.
pub fn fingerprints_from_scan(scanned: &[(std::path::PathBuf, u64)]) -> HashMap<String, u64> {
    scanned
        .iter()
        .filter(|(file, _)| file.extension().is_some_and(|e| e == "rs"))
        .map(|(file, mtime)| (file.display().to_string(), *mtime))
        .collect()
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;
    use crate::collector::collect_metadata;
    use crate::route_impl::StoredRouteInfo;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    //
    // The fast path matches `#[route]`'s `Span::local_file()` strings
    // (cwd-relative) against the collector's absolute walk paths.
    // Before normalization existed the keys NEVER matched and the
    // fast path was silently dead — every route file was re-parsed on
    // every cache miss with zero test failures.  These tests pin the
    // matching semantics so a regression is loud.

    #[rstest]
    // Relative path resolves against cwd → equals the absolute form.
    #[case("src/routes/users.rs", "/work/src/routes/users.rs", "/work")]
    // Separator style must not matter.
    #[case("src\\routes\\users.rs", "/work/src/routes/users.rs", "/work")]
    // `.` and `..` components fold on either side.
    #[case(
        "src/./routes/../routes/users.rs",
        "/work/src/routes/users.rs",
        "/work"
    )]
    #[case("src/routes/users.rs", "/work/extra/../src/routes/users.rs", "/work")]
    fn normalize_path_key_matches_equivalent_paths(
        #[case] stored: &str,
        #[case] walked: &str,
        #[case] cwd: &str,
    ) {
        let cwd = Path::new(cwd);
        assert_eq!(
            normalize_path_key(stored, cwd),
            normalize_path_key(walked, cwd),
            "stored={stored:?} and walked={walked:?} must produce the same key"
        );
    }

    #[test]
    fn normalize_path_key_distinguishes_different_files() {
        let cwd = Path::new("/work");
        assert_ne!(
            normalize_path_key("src/routes/users.rs", cwd),
            normalize_path_key("src/routes/posts.rs", cwd),
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_key_windows_verbatim_prefix_and_case() {
        let cwd = Path::new("C:\\work");
        // `fs::canonicalize` output style (\\?\ verbatim prefix) must
        // match plain absolute paths, and drive/file case must fold.
        assert_eq!(
            normalize_path_key("\\\\?\\C:\\Work\\Src\\Users.RS", cwd),
            normalize_path_key("c:/work/src/users.rs", cwd),
        );
    }

    /// END-TO-END lock for the fast-path activation bug: storage
    /// carries a **cwd-relative** path (exactly what
    /// `Span::local_file()` yields) while the collector walks an
    /// absolute folder.  The route file is deliberately INVALID Rust —
    /// the slow path would fail with a parse error, so a successful
    /// collect proves the fast path matched without parsing.
    #[test]
    fn fast_path_matches_cwd_relative_storage_paths_without_parsing() {
        // cargo runs tests with cwd = this crate's manifest dir, so a
        // path under the workspace `target/` dir has a stable relative
        // form mirroring rustc's span paths.
        let unique = format!("vespera_fastpath_lock_{}", std::process::id());
        let abs_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(&unique);
        fs::create_dir_all(&abs_dir).expect("create test route dir");
        fs::write(
            abs_dir.join("users.rs"),
            "this is deliberately not rust {{{",
        )
        .expect("write route file");

        let relative_stored_path = format!("../../target/{unique}/users.rs");
        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: None,
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
            fn_item_str: "pub async fn get_users() -> String { String::new() }".to_string(),
            file_path: Some(relative_stored_path),
        }];

        let result = collect_metadata(&abs_dir, "routes", &route_storage);
        fs::remove_dir_all(&abs_dir).ok();

        let (metadata, file_asts) = result.expect(
            "fast path must match the relative storage path WITHOUT parsing — \
             a parse error here means key normalization regressed and the \
             slow path ran against the invalid file",
        );
        assert_eq!(metadata.routes.len(), 1, "route must come from storage");
        assert!(
            file_asts.is_empty(),
            "fast path must not parse any file ASTs"
        );
    }

    /// Lock for the method-default bug: `#[route]` without a method
    /// stores `method: None`; the fast path must resolve it to "get"
    /// like the slow path does.  The original `unwrap_or_default()`
    /// produced "" — silently dropping such routes from the OpenAPI
    /// doc AND the generated router.
    #[test]
    fn fast_path_defaults_missing_method_to_get() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = create_temp_file(&temp_dir, "items.rs", "// placeholder\n");

        let route_storage = vec![StoredRouteInfo {
            fn_name: "list_items".to_string(),
            method: None, // bare `#[route]` / `#[route(path = ...)]`
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
            fn_item_str: "pub async fn list_items() -> String { String::new() }".to_string(),
            file_path: Some(file_path.display().to_string()),
        }];

        let (metadata, _) = collect_metadata(temp_dir.path(), "routes", &route_storage).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        assert_eq!(
            metadata.routes[0].method, "get",
            "missing method must default to GET — \"\" silently drops the route"
        );

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
    fn test_collect_metadata_fast_path_uses_stored_description() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let folder_name = "routes";

        let file_path = create_temp_file(&temp_dir, "items.rs", "// placeholder\n");

        let file_path_str = file_path.display().to_string();

        // `#[route]` resolves the description (explicit attribute OR doc
        // comment) at expansion time — see `process_route_attribute`.
        // The collector fast path must pass it through verbatim WITHOUT
        // re-parsing `fn_item_str`.
        let route_storage = vec![StoredRouteInfo {
            fn_name: "get_items".to_string(),
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
            description: Some("List all items".to_string()),
            fn_item_str:
                "/// List all items\npub async fn get_items() -> String { \"items\".to_string() }"
                    .to_string(),
            file_path: Some(file_path_str.clone()),
        }];

        let (metadata, _) = collect_metadata(temp_dir.path(), folder_name, &route_storage).unwrap();

        assert_eq!(metadata.routes.len(), 1);
        assert_eq!(
            metadata.routes[0].description,
            Some("List all items".to_string())
        );

        // A storage entry with no description stays None — the fast path
        // does NOT re-extract from fn_item_str (expansion already did).
        let route_storage_none = vec![StoredRouteInfo {
            fn_name: "get_items".to_string(),
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
            fn_item_str: "pub async fn get_items() -> String { \"items\".to_string() }".to_string(),
            file_path: Some(file_path_str),
        }];
        let (metadata, _) =
            collect_metadata(temp_dir.path(), folder_name, &route_storage_none).unwrap();
        assert_eq!(metadata.routes[0].description, None);

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

        let fingerprints = fingerprints_from_scan(&scan_route_folder(temp_dir.path()).unwrap());

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
