use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{MacroResult, err_call_site};

/// Name of the crate currently being expanded, for namespacing files
/// under the (workspace-shared) `target/vespera/` directory.  Two
/// workspace members both using `vespera!` would otherwise overwrite
/// each other's cache (permanent miss ping-pong) and — worse — race on
/// the shared spec file that the generated code `include_str!`s.
pub(super) fn current_crate_tag() -> String {
    let package =
        crate::env_access::var("CARGO_PKG_NAME").unwrap_or_else(|_| "default".to_string());
    // A package's binaries are separate crates expanding the same macros in
    // parallel with its library: give each its own build files.
    match crate::env_access::var("CARGO_CRATE_NAME") {
        Ok(krate) if krate != package.replace('-', "_") => format!("{package}.{krate}"),
        _ => package,
    }
}

/// `info.description` default: the crate's `description` from Cargo.toml
/// (`CARGO_PKG_DESCRIPTION`), when non-blank.
pub(super) fn package_description() -> Option<String> {
    crate::env_access::var("CARGO_PKG_DESCRIPTION")
        .ok()
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty())
}

/// Find the folder path for route scanning
pub fn find_folder_path(folder_name: &str) -> MacroResult<std::path::PathBuf> {
    let root = crate::env_access::var("CARGO_MANIFEST_DIR").map_err(|_| {
        err_call_site(
            "CARGO_MANIFEST_DIR is not set. vespera macros must be used within a cargo build.",
        )
    })?;
    // The macro parsers only admit `src/`-relative module folders
    // (`router_codegen::validate_route_dir`); an absolute path is accepted
    // here for embedders and tests that point at a folder directly.  A
    // relative folder is never looked up against the working directory: a
    // missing `src/<dir>` is reported as such instead of silently scanning
    // whatever `<cwd>/<dir>` happens to be.
    if Path::new(folder_name).is_absolute() {
        return Ok(PathBuf::from(folder_name));
    }
    Ok(Path::new(&root).join("src").join(folder_name))
}

/// Resolve a user-supplied output path (`openapi = "..."`) against the
/// crate's `CARGO_MANIFEST_DIR`, so `openapi = "openapi.json"` lands next to
/// the crate's `Cargo.toml` no matter which directory rustc runs in (cargo
/// runs workspace members from the workspace root). Absolute paths are kept;
/// without a manifest directory the path is returned unchanged.
pub fn resolve_manifest_relative(path: &str) -> PathBuf {
    resolve_against(
        crate::env_access::var_os("CARGO_MANIFEST_DIR"),
        Path::new(path),
    )
}

fn resolve_against(base: Option<OsString>, path: &Path) -> PathBuf {
    match base {
        Some(base) if path.is_relative() => Path::new(&base).join(path),
        _ => path.to_path_buf(),
    }
}

thread_local! {
    /// Resolved workspace roots keyed by the manifest path that started the
    /// walk.  The workspace layout is fixed within a build (and across
    /// invocations in a long-lived proc-macro server), so a root resolved
    /// once stays valid — this avoids re-walking ancestors and re-reading
    /// each `Cargo.toml` on every `vespera!` / sidecar-path call.  Mirrors
    /// `file_cache`'s process-lifetime `manifest_dir` cache.
    static WORKSPACE_ROOT_CACHE: RefCell<HashMap<PathBuf, PathBuf>> =
        RefCell::new(HashMap::new());
}

/// The cargo target directory for the crate at `manifest_path`.
///
/// Honours `CARGO_TARGET_DIR` (a relative value is taken relative to the
/// workspace root, the directory cargo runs rustc from); otherwise
/// `<workspace root>/target`.
pub fn find_target_dir(manifest_path: &Path) -> PathBuf {
    target_dir_in(
        &find_workspace_root(manifest_path),
        crate::env_access::var_os("CARGO_TARGET_DIR"),
    )
}

/// `CARGO_TARGET_DIR` (when non-empty; an absolute value replaces the root
/// on `join`) or `<workspace root>/target`.
fn target_dir_in(workspace_root: &Path, cargo_target_dir: Option<OsString>) -> PathBuf {
    match cargo_target_dir {
        Some(target_dir) if !target_dir.is_empty() => workspace_root.join(target_dir),
        _ => workspace_root.join("target"),
    }
}

/// The workspace root of the crate at `manifest_path` (cached per path).
pub fn find_workspace_root(manifest_path: &Path) -> PathBuf {
    if let Some(cached) = WORKSPACE_ROOT_CACHE.with(|c| c.borrow().get(manifest_path).cloned()) {
        return cached;
    }
    let resolved = find_workspace_root_uncached(manifest_path);
    WORKSPACE_ROOT_CACHE.with(|c| {
        c.borrow_mut()
            .insert(manifest_path.to_path_buf(), resolved.clone());
    });
    resolved
}

fn find_workspace_root_uncached(manifest_path: &Path) -> PathBuf {
    // Look for workspace root by finding a Cargo.toml with a `workspace` table
    let mut current = Some(manifest_path);
    let mut last_with_lock = None;

    while let Some(dir) = current {
        // Check if this directory has Cargo.lock
        if dir.join("Cargo.lock").exists() {
            last_with_lock = Some(dir.to_path_buf());
        }

        // `read_to_string` already fails when the file does not exist, so no
        // `.exists()` pre-flight is needed.
        if let Ok(contents) = std::fs::read_to_string(dir.join("Cargo.toml"))
            && declares_workspace(&contents)
        {
            return dir.to_path_buf();
        }

        current = dir.parent();
    }

    // No workspace manifest: the topmost directory holding a Cargo.lock,
    // else the crate itself.
    last_with_lock.unwrap_or_else(|| manifest_path.to_path_buf())
}

/// Whether a `Cargo.toml` declares a `[workspace]` table — a table header
/// `[workspace]` / `[workspace.<sub>]` (whitespace inside the brackets
/// allowed) at the start of a line, or a top-level `workspace = ...` key.
/// Line-anchored so `workspace` inside a string, comment or another key
/// (`"[workspace]"`, `# [workspace]`, `[package.workspace]`) does not match.
fn declares_workspace(cargo_toml: &str) -> bool {
    let mut top_level = true;
    for line in cargo_toml.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if let Some(header) = line.strip_prefix('[') {
            let name = header.split(']').next().unwrap_or_default();
            if name.split('.').next().unwrap_or_default().trim() == "workspace" {
                return true;
            }
            top_level = false;
        } else if top_level
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "workspace")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_find_folder_path_nonexistent_returns_path() {
        // A missing folder still resolves under `src/` — never against the
        // working directory — so the caller reports `src/<dir>` as missing.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result = find_folder_path("nonexistent_folder_xyz").unwrap();
        assert_eq!(
            result,
            temp_dir.path().join("src").join("nonexistent_folder_xyz")
        );
        // Nested module folders resolve the same way.
        assert_eq!(
            find_folder_path("api/v1").unwrap(),
            temp_dir.path().join("src").join("api/v1")
        );
    }

    // ========== Tests for find_workspace_root ==========

    #[test]
    fn test_find_workspace_root_no_workspace() {
        // Test fallback to manifest dir's target
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();
        let result = find_workspace_root(manifest_path);
        assert_eq!(result, manifest_path);
    }

    #[test]
    fn test_find_workspace_root_with_cargo_lock() {
        // Test finding target dir with Cargo.lock present
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();

        // Create Cargo.lock (but no [workspace] in Cargo.toml)
        fs::write(manifest_path.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");

        let result = find_workspace_root(manifest_path);
        // Should use the directory with Cargo.lock
        assert_eq!(result, manifest_path);
    }

    #[test]
    fn test_find_workspace_root_with_workspace() {
        // Test finding workspace root
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace_root = temp_dir.path();

        // Create a workspace Cargo.toml
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate1\"]",
        )
        .expect("Failed to write Cargo.toml");

        // Create nested crate directory
        let crate_dir = workspace_root.join("crate1");
        fs::create_dir(&crate_dir).expect("Failed to create crate dir");
        fs::write(crate_dir.join("Cargo.toml"), "[package]\nname = \"crate1\"")
            .expect("Failed to write Cargo.toml");

        let result = find_workspace_root(&crate_dir);
        // Should return workspace root's target
        assert_eq!(result, workspace_root);
    }

    #[test]
    fn test_find_workspace_root_workspace_with_cargo_lock() {
        // Test that [workspace] takes priority over Cargo.lock
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace_root = temp_dir.path();

        // Create workspace Cargo.toml and Cargo.lock
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate1\"]",
        )
        .expect("Failed to write Cargo.toml");
        fs::write(workspace_root.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");

        // Create nested crate
        let crate_dir = workspace_root.join("crate1");
        fs::create_dir(&crate_dir).expect("Failed to create crate dir");
        fs::write(crate_dir.join("Cargo.toml"), "[package]\nname = \"crate1\"")
            .expect("Failed to write Cargo.toml");

        let result = find_workspace_root(&crate_dir);
        assert_eq!(result, workspace_root);
    }

    #[test]
    fn test_find_workspace_root_deeply_nested() {
        // Test deeply nested crate structure
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace_root = temp_dir.path();

        // Create workspace
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]",
        )
        .expect("Failed to write Cargo.toml");

        // Create deeply nested crate
        let deep_crate = workspace_root.join("crates/group/my-crate");
        fs::create_dir_all(&deep_crate).expect("Failed to create nested dirs");
        fs::write(deep_crate.join("Cargo.toml"), "[package]").expect("Failed to write Cargo.toml");

        let result = find_workspace_root(&deep_crate);
        assert_eq!(result, workspace_root);
    }

    #[rstest::rstest]
    #[case::table("[workspace]\nmembers = []", true)]
    #[case::spaced_table("[ workspace ]", true)]
    #[case::sub_table_only("[workspace.package]\nversion = \"1\"", true)]
    #[case::trailing_comment("[workspace] # root", true)]
    #[case::inline_key("workspace = { members = [] }\n[package]", true)]
    #[case::member_pointer("[package]\nname = \"a\"\nworkspace = \"..\"", false)]
    #[case::commented_out("# [workspace]\n[package]", false)]
    #[case::string_value("[package]\ndescription = \"[workspace]\"", false)]
    #[case::other_table("[package.workspace]", false)]
    #[case::array_table("[[bin]]\nname = \"x\"", false)]
    #[case::empty("", false)]
    fn declares_workspace_is_line_anchored(#[case] toml: &str, #[case] expected: bool) {
        assert_eq!(declares_workspace(toml), expected);
    }

    #[test]
    fn workspace_package_table_marks_the_root() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"1\"",
        )
        .unwrap();
        let member = root.join("member");
        fs::create_dir(&member).unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nworkspace = \"..\"",
        )
        .unwrap();
        assert_eq!(find_workspace_root(&member), root);
    }

    #[rstest::rstest]
    #[case::unset(None, "root/target")]
    #[case::empty(Some(""), "root/target")]
    #[case::relative(Some("out/tgt"), "root/out/tgt")]
    fn target_dir_honours_cargo_target_dir(#[case] env: Option<&str>, #[case] expected: &str) {
        assert_eq!(
            target_dir_in(Path::new("root"), env.map(OsString::from)),
            Path::new(expected)
        );
    }

    #[test]
    fn absolute_cargo_target_dir_replaces_the_root() {
        let absolute = std::env::temp_dir().join("vespera-target");
        assert_eq!(
            target_dir_in(Path::new("root"), Some(absolute.clone().into_os_string())),
            absolute
        );
    }

    #[test]
    fn find_target_dir_is_inside_the_workspace_root_by_default() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let expected = target_dir_in(
            temp_dir.path(),
            crate::env_access::var_os("CARGO_TARGET_DIR"),
        );
        assert_eq!(find_target_dir(temp_dir.path()), expected);
    }

    #[test]
    fn output_paths_resolve_against_the_manifest_dir() {
        let absolute = std::env::temp_dir().join("spec.json");
        assert_eq!(
            resolve_against(Some(OsString::from("crate")), Path::new("openapi.json")),
            Path::new("crate").join("openapi.json")
        );
        assert_eq!(
            resolve_against(Some(OsString::from("crate")), &absolute),
            absolute
        );
        assert_eq!(
            resolve_against(None, Path::new("openapi.json")),
            Path::new("openapi.json")
        );
        assert!(resolve_manifest_relative("openapi.json").ends_with("openapi.json"));
    }

    #[test]
    fn test_find_folder_path_absolute_path() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let absolute_path = temp_dir.path().to_string_lossy().to_string();

        // When given an absolute path that exists, it should return it
        let result = find_folder_path(&absolute_path).unwrap();
        // The function tries src/{folder_name} first, then falls back to the folder_name directly
        assert!(
            result.to_string_lossy().contains(&absolute_path)
                || result == Path::new(&absolute_path)
        );
    }

    #[test]
    fn test_find_folder_path_with_src_folder() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create src/routes directory
        let src_routes = temp_dir.path().join("src").join("routes");
        fs::create_dir_all(&src_routes).expect("Failed to create src/routes dir");

        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());

        let result = find_folder_path("routes").unwrap();

        // Should return the src/routes path since it exists
        assert!(
            result.to_string_lossy().contains("src") && result.to_string_lossy().contains("routes")
        );
    }

    /// A binary named differently from its package gets its own tag; the
    /// library (and its test build) keep the package name.
    #[rstest::rstest]
    #[case::library(Some("my_app"), "my-app")]
    #[case::binary(Some("server"), "my-app.server")]
    #[case::outside_cargo(None, "my-app")]
    fn crate_tag_separates_binaries(#[case] krate: Option<&str>, #[case] expected: &str) {
        let _pkg = crate::env_access::test_env::set_var("CARGO_PKG_NAME", "my-app");
        let _krate = krate.map_or_else(
            || crate::env_access::test_env::remove_var("CARGO_CRATE_NAME"),
            |krate| crate::env_access::test_env::set_var("CARGO_CRATE_NAME", krate),
        );
        assert_eq!(current_crate_tag(), expected);
    }

    #[test]
    fn find_folder_path_reports_missing_manifest_environment() {
        let _env_guard = crate::env_access::test_env::remove_var("CARGO_MANIFEST_DIR");
        let error = find_folder_path("routes").expect_err("manifest directory is required");
        assert!(error.to_string().contains("CARGO_MANIFEST_DIR is not set"));
    }

    #[rstest::rstest]
    #[case::described(Some("  Pet store API "), Some("Pet store API"))]
    #[case::blank(Some("   "), None)]
    #[case::unset(None, None)]
    fn package_description_reads_cargo_description(
        #[case] value: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        let _env_guard = value.map_or_else(
            || crate::env_access::test_env::remove_var("CARGO_PKG_DESCRIPTION"),
            |value| crate::env_access::test_env::set_var("CARGO_PKG_DESCRIPTION", value),
        );
        assert_eq!(package_description().as_deref(), expected);
    }
}
