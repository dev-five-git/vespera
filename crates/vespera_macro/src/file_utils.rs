use std::{
    io,
    path::{Path, PathBuf},
};

/// Read and parse a Rust source file, printing warnings on error.
#[allow(clippy::similar_names)]
pub fn read_and_parse_file_warn(path: &Path, context: &str) -> Option<syn::File> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Warning: {}: Cannot read '{}': {}",
                context,
                path.display(),
                e
            );
            return None;
        }
    };
    match syn::parse_file(&content) {
        Ok(ast) => Some(ast),
        Err(e) => {
            eprintln!(
                "Warning: {}: Parse error in '{}': {}",
                context,
                path.display(),
                e
            );
            None
        }
    }
}

pub fn collect_files(folder_path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder_path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            files.push(folder_path.join(path));
        } else if path.is_dir() {
            files.extend(collect_files(&folder_path.join(&path))?);
        }
    }
    Ok(files)
}

pub fn file_to_segments(file: &Path, base_path: &Path) -> Vec<String> {
    let file_stem = file.strip_prefix(base_path).map_or_else(
        |_| file.display().to_string(),
        |file_stem| file_stem.display().to_string(),
    );
    let file_stem = file_stem.replace(".rs", "").replace('\\', "/");
    let mut segments: Vec<String> = file_stem
        .split('/')
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
        .collect();
    if let Some(last) = segments.last()
        && last == "mod"
    {
        segments.pop();
    }
    segments
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    #[rstest]
    // Simple file paths
    #[case("routes/users.rs", "routes", vec!["users"])]
    #[case("routes/posts.rs", "routes", vec!["posts"])]
    #[case("routes/users.rs", "routes/", vec!["users"])]
    // Nested directories
    #[case("routes/admin/users.rs", "routes", vec!["admin", "users"])]
    #[case("routes/api/v1/users.rs", "routes", vec!["api", "v1", "users"])]
    #[case("routes/admin/settings.rs", "routes", vec!["admin", "settings"])]
    // Deep nesting
    #[case("routes/api/v1/users/profile.rs", "routes", vec!["api", "v1", "users", "profile"])]
    // mod.rs files
    #[case("routes/mod.rs", "routes", vec![])]
    #[case("routes/admin/mod.rs", "routes", vec!["admin"])]
    #[case("routes/api/v1/mod.rs", "routes", vec!["api", "v1"])]
    // mod in middle (should not be removed)
    #[case("routes/mod_users.rs", "routes", vec!["mod_users"])]
    // Windows-style paths (backslashes)
    #[case("routes\\users.rs", "routes", vec!["users"])]
    #[case("routes\\admin\\users.rs", "routes", vec!["admin", "users"])]
    #[case("routes\\mod.rs", "routes", vec![])]
    // Files without .rs extension (should still work)
    #[case("routes/users", "routes", vec!["users"])]
    #[case("routes/admin/users", "routes", vec!["admin", "users"])]
    // Empty segments
    #[case("routes//users.rs", "routes", vec!["users"])]
    #[case("routes///admin//users.rs", "routes", vec!["admin", "users"])]
    // Base path not matching
    #[case("/absolute/path/users.rs", "routes", vec!["absolute", "path", "users"])]
    #[case("different/path/users.rs", "routes", vec!["different", "path", "users"])]
    // Root level files
    #[case("users.rs", ".", vec!["users"])]
    #[case("mod.rs", ".", vec![])]
    fn test_file_to_segments(
        #[case] file_path: &str,
        #[case] base_path: &str,
        #[case] expected: Vec<&str>,
    ) {
        // Normalize paths by replacing backslashes with forward slashes
        // This ensures tests work cross-platform (Windows uses \, Unix uses /)
        let normalized_file_path = file_path.replace('\\', "/");
        let normalized_base_path = base_path.replace('\\', "/");
        let file = PathBuf::from(normalized_file_path);
        let base = PathBuf::from(normalized_base_path);
        let result = file_to_segments(&file, &base);
        let expected_vec: Vec<String> = expected
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(
            result, expected_vec,
            "Failed for file: {file_path}, base: {base_path}"
        );
    }

    fn create_test_structure(
        temp_dir: &TempDir,
        structure: &[(&str, bool)],
    ) -> Result<(), std::io::Error> {
        // (path, is_file)
        for (path, is_file) in structure {
            let full_path = temp_dir.path().join(path);
            if *is_file {
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&full_path, "test content")?;
            } else {
                fs::create_dir_all(&full_path)?;
            }
        }
        Ok(())
    }

    fn normalize_paths(paths: &[PathBuf], base: &Path) -> Vec<String> {
        let mut normalized: Vec<String> = paths
            .iter()
            .map(|p| {
                p.strip_prefix(base)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        normalized.sort();
        normalized
    }

    #[rstest]
    // Empty directory
    #[case(vec![], vec![])]
    // Single file
    #[case(vec![("file1.rs", true)], vec!["file1.rs"])]
    // Multiple files in root
    #[case(
        vec![("file1.rs", true), ("file2.rs", true), ("file3.rs", true)],
        vec!["file1.rs", "file2.rs", "file3.rs"]
    )]
    // Single nested directory with file
    #[case(
        vec![("subdir", false), ("subdir/file.rs", true)],
        vec!["subdir/file.rs"]
    )]
    // Multiple nested directories
    #[case(
        vec![
            ("dir1", false),
            ("dir1/file1.rs", true),
            ("dir2", false),
            ("dir2/file2.rs", true),
        ],
        vec!["dir1/file1.rs", "dir2/file2.rs"]
    )]
    // Deep nesting
    #[case(
        vec![
            ("a", false),
            ("a/b", false),
            ("a/b/c", false),
            ("a/b/c/file.rs", true),
        ],
        vec!["a/b/c/file.rs"]
    )]
    // Mixed structure
    #[case(
        vec![
            ("root.rs", true),
            ("dir1", false),
            ("dir1/file1.rs", true),
            ("dir1/file2.rs", true),
            ("dir2", false),
            ("dir2/subdir", false),
            ("dir2/subdir/file.rs", true),
        ],
        vec!["dir1/file1.rs", "dir1/file2.rs", "dir2/subdir/file.rs", "root.rs"]
    )]
    // Files with different extensions
    #[case(
        vec![
            ("file.rs", true),
            ("file.txt", true),
            ("file.md", true),
        ],
        vec!["file.md", "file.rs", "file.txt"]
    )]
    // Empty subdirectories (should be ignored)
    #[case(
        vec![
            ("empty_dir", false),
            ("file.rs", true),
        ],
        vec!["file.rs"]
    )]
    fn test_collect_files(#[case] structure: Vec<(&str, bool)>, #[case] expected_files: Vec<&str>) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_test_structure(&temp_dir, &structure).expect("Failed to create test structure");

        let result = collect_files(temp_dir.path()).expect("collect_files failed");
        let mut normalized_result = normalize_paths(&result, temp_dir.path());
        normalized_result.sort();

        let mut expected_normalized: Vec<String> = expected_files
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        expected_normalized.sort();

        assert_eq!(
            normalized_result, expected_normalized,
            "Failed for structure: {structure:?}"
        );

        temp_dir.close().expect("Failed to close temp dir");
    }

    #[test]
    fn test_collect_files_nonexistent_directory() {
        let nonexistent = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let result = collect_files(&nonexistent);
        assert!(result.is_err());
    }

    #[test]
    fn test_collect_files_recursive_deep() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create a very deep nested structure
        let mut path = temp_dir.path().to_path_buf();
        for i in 0..5 {
            path = path.join(format!("level{i}"));
            fs::create_dir_all(&path).expect("Failed to create nested dir");
        }

        // Create a file at the deepest level
        let file_path = path.join("deep_file.rs");
        fs::write(&file_path, "content").expect("Failed to write file");

        let result = collect_files(temp_dir.path()).expect("collect_files failed");
        assert_eq!(result.len(), 1);
        assert!(result[0].ends_with("deep_file.rs"));

        temp_dir.close().expect("Failed to close temp dir");
    }
}
