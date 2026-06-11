use std::path::Path;

use crate::error::{MacroResult, err_call_site};

/// Name of the crate currently being expanded, for namespacing files
/// under the (workspace-shared) `target/vespera/` directory.  Two
/// workspace members both using `vespera!` would otherwise overwrite
/// each other's cache (permanent miss ping-pong) and — worse — race on
/// the shared spec file that the generated code `include_str!`s.
pub(super) fn current_crate_tag() -> String {
    std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "default".to_string())
}

/// Find the folder path for route scanning
pub fn find_folder_path(folder_name: &str) -> MacroResult<std::path::PathBuf> {
    let root = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        err_call_site(
            "CARGO_MANIFEST_DIR is not set. vespera macros must be used within a cargo build.",
        )
    })?;
    let path = format!("{root}/src/{folder_name}");
    let path = Path::new(&path);
    if path.exists() && path.is_dir() {
        return Ok(path.to_path_buf());
    }

    Ok(Path::new(folder_name).to_path_buf())
}

/// Find the workspace root's target directory
pub fn find_target_dir(manifest_path: &Path) -> std::path::PathBuf {
    // Look for workspace root by finding a Cargo.toml with [workspace] section
    let mut current = Some(manifest_path);
    let mut last_with_lock = None;

    while let Some(dir) = current {
        // Check if this directory has Cargo.lock
        if dir.join("Cargo.lock").exists() {
            last_with_lock = Some(dir.to_path_buf());
        }

        // Check if this is a workspace root (has Cargo.toml with [workspace]).
        // `read_to_string` already fails when the file does not exist, so the
        // previous `.exists()` pre-flight is redundant — drop it to save one
        // stat per iteration of the walk.
        if let Ok(contents) = std::fs::read_to_string(dir.join("Cargo.toml"))
            && contents.contains("[workspace]")
        {
            return dir.join("target");
        }

        current = dir.parent();
    }

    // If we found a Cargo.lock but no [workspace], use the topmost one
    if let Some(lock_dir) = last_with_lock {
        return lock_dir.join("target");
    }

    // Fallback: use manifest dir's target
    manifest_path.join("target")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_find_folder_path_nonexistent_returns_path() {
        // When the constructed path doesn't exist, it falls back to using folder_name directly
        let result = find_folder_path("nonexistent_folder_xyz").unwrap();
        // It should return a PathBuf (either from src/nonexistent... or just the folder name)
        assert!(result.to_string_lossy().contains("nonexistent_folder_xyz"));
    }

    // ========== Tests for find_target_dir ==========

    #[test]
    fn test_find_target_dir_no_workspace() {
        // Test fallback to manifest dir's target
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();
        let result = find_target_dir(manifest_path);
        assert_eq!(result, manifest_path.join("target"));
    }

    #[test]
    fn test_find_target_dir_with_cargo_lock() {
        // Test finding target dir with Cargo.lock present
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();

        // Create Cargo.lock (but no [workspace] in Cargo.toml)
        fs::write(manifest_path.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");

        let result = find_target_dir(manifest_path);
        // Should use the directory with Cargo.lock
        assert_eq!(result, manifest_path.join("target"));
    }

    #[test]
    fn test_find_target_dir_with_workspace() {
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

        let result = find_target_dir(&crate_dir);
        // Should return workspace root's target
        assert_eq!(result, workspace_root.join("target"));
    }

    #[test]
    fn test_find_target_dir_workspace_with_cargo_lock() {
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

        let result = find_target_dir(&crate_dir);
        assert_eq!(result, workspace_root.join("target"));
    }

    #[test]
    fn test_find_target_dir_deeply_nested() {
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

        let result = find_target_dir(&deep_crate);
        assert_eq!(result, workspace_root.join("target"));
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

    #[serial_test::serial]
    #[test]
    fn test_find_folder_path_with_src_folder() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create src/routes directory
        let src_routes = temp_dir.path().join("src").join("routes");
        fs::create_dir_all(&src_routes).expect("Failed to create src/routes dir");

        // Save and set CARGO_MANIFEST_DIR
        let old_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: We're in a single-threaded test context
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let result = find_folder_path("routes").unwrap();

        // Restore CARGO_MANIFEST_DIR
        if let Some(old_value) = old_manifest_dir {
            // SAFETY: We're in a single-threaded test context
            unsafe { std::env::set_var("CARGO_MANIFEST_DIR", old_value) };
        }

        // Should return the src/routes path since it exists
        assert!(
            result.to_string_lossy().contains("src") && result.to_string_lossy().contains("routes")
        );
    }
}
