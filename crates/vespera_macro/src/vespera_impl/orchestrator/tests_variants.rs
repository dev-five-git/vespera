//! One crate, several concurrently compiled variants: the library, its
//! `--test` harness and rust-analyzer expand the same `vespera!`.

use std::fs;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::*;
use crate::env_access::{COMPILER_ARGS_KEY, test_env};

/// The `include_str!("...")` target in generated tokens.
fn include_target(tokens: &str) -> PathBuf {
    let start = tokens.find("include_str ! (\"").expect("include_str!") + "include_str ! (\"".len();
    let end = start + tokens[start..].find('"').unwrap();
    PathBuf::from(tokens[start..end].replace("\\\\", "\\"))
}

fn crate_with_routes(users: &str) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let routes = temp_dir.path().join("src").join("routes");
    fs::create_dir_all(&routes).unwrap();
    fs::write(routes.join("users.rs"), users).unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    temp_dir
}

fn served(folder: &str) -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: folder.to_string(),
        spec_url: Some("/openapi.json".to_string()),
        openapi_file_names: vec!["openapi.json".to_string()],
        ..ProcessedVesperaInput::default()
    }
}

/// The test harness build generates another spec than the library; it must
/// never prune the library's `include_str!` target, however old that is
/// (it is never rewritten while its content is unchanged).
#[test]
fn test_harness_build_never_prunes_the_library_sidecar() {
    let temp_dir = crate_with_routes("#[vespera::route(get)] pub async fn list() {}\n");
    let _manifest = test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let _pkg = test_env::set_var("CARGO_PKG_NAME", "variants");
    let _krate = test_env::set_var("CARGO_CRATE_NAME", "variants");
    let _bin = test_env::remove_var("CARGO_BIN_NAME");

    let library = {
        let _args = test_env::set_var(COMPILER_ARGS_KEY, "rustc");
        process_vespera_macro(&served("routes"), &HashMap::new(), &[], Span::call_site())
            .unwrap()
            .to_string()
    };
    let library_spec = include_target(&library);
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&library_spec)
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();

    fs::write(
        temp_dir.path().join("src/routes/users.rs"),
        "#[vespera::route(get)] pub async fn list() {}\n#[vespera::route(post)] pub async fn create() {}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let harness = {
        let _args = test_env::set_var(COMPILER_ARGS_KEY, "rustc\n--test");
        process_vespera_macro(&served("routes"), &HashMap::new(), &[], Span::call_site())
            .unwrap()
            .to_string()
    };
    let harness_spec = include_target(&harness);
    assert_ne!(library_spec, harness_spec);
    assert!(harness_spec.exists());
    assert!(
        library_spec.exists(),
        "the test harness build pruned {}",
        library_spec.display()
    );
}

const USERS: &str = "\
#[vespera::route(get)]
pub async fn list() {}
#[cfg(test)]
#[vespera::route(get, path = \"/debug\")]
pub async fn debug() {}
#[cfg(test)]
mod tests {
    pub struct Fixture { pub id: i32 }
}
";

/// `openapi.json` is identical whether the library (test items compiled
/// out) or the test harness / rust-analyzer (test items registered in
/// `ROUTE_STORAGE` / `SCHEMA_STORAGE`) wrote it last; the harness router
/// still mounts the test-only route.
#[test]
fn openapi_json_does_not_depend_on_cfg_test_items() {
    let temp_dir = crate_with_routes(USERS);
    let _manifest = test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let users = temp_dir.path().join("src/routes/users.rs");
    let openapi = temp_dir.path().join("openapi.json");

    {
        let _args = test_env::set_var(COMPILER_ARGS_KEY, "rustc");
        process_vespera_macro(&served("routes"), &HashMap::new(), &[], Span::call_site()).unwrap();
    }
    let library_doc = fs::read_to_string(&openapi).unwrap();
    assert!(!library_doc.contains("/users/debug"), "{library_doc}");

    let stored = |name: &str, path: Option<&str>| crate::route_impl::StoredRouteInfo {
        fn_name: name.to_string(),
        method: Some("get".to_string()),
        custom_path: path.map(str::to_string),
        file_path: Some(users.display().to_string()),
        fn_sig_str: format!("async fn {name}()"),
        inline_module: Some(String::new()),
        ..Default::default()
    };
    let route_storage = [stored("list", None), stored("debug", Some("/debug"))];
    let fixture = StructMetadata::new(
        "Fixture".to_string(),
        "pub struct Fixture { pub id: i32 }".to_string(),
    )
    .with_source_identity(format!("{}::Fixture@8:15", users.display()));
    let schema_storage = HashMap::from([("Fixture".to_string(), fixture)]);
    let harness = {
        let _args = test_env::set_var(COMPILER_ARGS_KEY, "rustc\n--test");
        process_vespera_macro(
            &served("routes"),
            &schema_storage,
            &route_storage,
            Span::call_site(),
        )
        .unwrap()
        .to_string()
    };
    assert_eq!(fs::read_to_string(&openapi).unwrap(), library_doc);
    assert!(harness.contains("users :: debug"), "{harness}");

    // Control: the same stored schema outside the gated module (line 2) is
    // documented, so the equality above really filtered `Fixture` out.
    let open_fixture = StructMetadata::new(
        "Fixture".to_string(),
        "pub struct Fixture { pub id: i32 }".to_string(),
    )
    .with_source_identity(format!("{}::Fixture@2:0", users.display()));
    {
        let _args = test_env::set_var(COMPILER_ARGS_KEY, "rustc\n--test");
        process_vespera_macro(
            &served("routes"),
            &HashMap::from([("Fixture".to_string(), open_fixture)]),
            &route_storage,
            Span::call_site(),
        )
        .unwrap();
    }
    let open_doc = fs::read_to_string(&openapi).unwrap();
    assert!(open_doc.contains("\"Fixture\""), "{open_doc}");
}

/// A `mod` declaration in `src/lib.rs` gating a schema's file or the route
/// folder on `cfg(test)` invalidates the cached document even though no
/// route file and no stored item changed (the crate root is not
/// fingerprinted; the test-only verdicts are part of the cache key).
#[test]
fn gating_a_module_in_the_crate_root_invalidates_the_cache() {
    let temp_dir = crate_with_routes("#[vespera::route(get)] pub async fn list() {}\n");
    let src = temp_dir.path().join("src");
    let _manifest = test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let _args = test_env::set_var(COMPILER_ARGS_KEY, "rustc\n--test");
    fs::write(src.join("routes/debug.rs"), "").unwrap();
    fs::write(
        src.join("fixtures.rs"),
        "pub struct Fixture { pub id: i32 }\n",
    )
    .unwrap();
    let fixture = StructMetadata::new(
        "Fixture".to_string(),
        "pub struct Fixture { pub id: i32 }".to_string(),
    )
    .with_source_identity(format!(
        "{}::Fixture@1:11",
        src.join("fixtures.rs").display()
    ));
    let schema_storage = HashMap::from([("Fixture".to_string(), fixture)]);
    let route_storage = [crate::route_impl::StoredRouteInfo {
        fn_name: "probe".to_string(),
        method: Some("get".to_string()),
        file_path: Some(src.join("routes/debug.rs").display().to_string()),
        fn_sig_str: "async fn probe()".to_string(),
        inline_module: Some(String::new()),
        ..Default::default()
    }];
    let document = |lib: &str| {
        fs::write(src.join("lib.rs"), lib).unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        process_vespera_macro(
            &served("routes"),
            &schema_storage,
            &route_storage,
            Span::call_site(),
        )
        .unwrap();
        fs::read_to_string(temp_dir.path().join("openapi.json")).unwrap()
    };

    let open = document("mod routes; mod fixtures;");
    assert!(
        open.contains("Fixture") && open.contains("/debug"),
        "{open}"
    );
    let gated = document("#[cfg(test)] mod routes; #[cfg(test)] mod fixtures;");
    assert!(
        !gated.contains("Fixture") && !gated.contains("/debug"),
        "{gated}"
    );
}
