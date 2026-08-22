use std::{
    io,
    path::{Path, PathBuf},
};

/// Render a path for compile-time strings and diagnostics with `/` separators.
///
/// `Path::display()` renders exactly the same lossy UTF-8 text as
/// [`Path::to_string_lossy`], so going through the `Cow` lets the common
/// separator-free case (every Unix path, and any Windows path already spelled
/// with `/`) finish in ONE allocation instead of the two that
/// `display().to_string().replace(..)` always paid — and a path that is already
/// valid UTF-8 borrows, so `into_owned()` is the only copy. Callers run this
/// once per route file per `vespera!` expansion (`collector.rs`,
/// [`normalize_path_key`], [`file_to_segments`], `orchestrator.rs`,
/// `openapi_io.rs`), so the saved allocation is per-file, per-build.
pub fn normalize_display_path(path: impl AsRef<Path>) -> String {
    let rendered = path.as_ref().to_string_lossy();
    if rendered.contains('\\') {
        rendered.replace('\\', "/")
    } else {
        rendered.into_owned()
    }
}

/// Compare two optional source paths treating `\` and `/` as equivalent,
/// WITHOUT allocating a normalized copy of either side.
///
/// Route and cron registration call this once per already-registered item on
/// every attribute expansion. Folding `\` to `/` byte-by-byte removes the two
/// `String` allocations from `.replace('\\', "/")` per comparison while keeping
/// the previous comparison semantics exactly.
pub fn paths_equal_normalized(left: Option<&str>, right: Option<&str>) -> bool {
    let (left, right) = (left.unwrap_or_default(), right.unwrap_or_default());
    let norm = |b: u8| if b == b'\\' { b'/' } else { b };
    left.len() == right.len()
        && std::iter::zip(left.bytes(), right.bytes())
            .all(|(left, right)| norm(left) == norm(right))
}

/// Normalize a path string into a comparison key **without touching the filesystem**.
///
/// Relative paths are absolutized against `cwd`, `.`/`..` components are folded,
/// separators normalize to `/`, the Windows `\\?\` verbatim prefix is stripped,
/// and (Windows only) the drive letter case is folded.
pub fn normalize_path_key(path: &str, cwd: &Path) -> String {
    use std::path::Component;

    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let mut folded = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    let mut key = normalize_display_path(&folded);
    if let Some(stripped) = key.strip_prefix("//?/") {
        key = stripped.to_owned();
    }
    if cfg!(windows) {
        key.make_ascii_lowercase();
    }
    key
}

/// Render a path for use in `include_str!` literals.
pub fn path_to_include_str_literal(path: impl AsRef<Path>) -> String {
    normalize_display_path(path)
}

// `#[cfg(test)]`: the only caller left is the test-only `collector::collect_metadata`
// (plus this module's own tests); production scanning goes through
// `collect_files_with_mtimes`, so the path-only wrapper never ships.
#[cfg(test)]
pub fn collect_files(folder_path: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(collect_files_with_mtimes(folder_path)?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

/// Recursively collect files together with their mtime fingerprints
/// (nanoseconds since `UNIX_EPOCH`; `0` when unavailable).
///
/// One walk serves both route discovery and cache fingerprinting —
/// previously the folder was walked twice and every file paid an
/// extra `fs::metadata` syscall on top of the directory-entry data
/// the OS already returned.
pub fn collect_files_with_mtimes(folder_path: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let mut files = Vec::new();
    collect_with_mtimes_into(folder_path, &mut files)?;
    Ok(files)
}

/// Compile-time cache fingerprint for a source file's modification time.
///
/// Uses **nanosecond** resolution rather than whole seconds: two edits to
/// the same file within one wall-clock second — routine under fast
/// incremental rebuilds and long-lived rust-analyzer processes — still
/// yield distinct fingerprints, so a stale router / OpenAPI spec is never
/// served from the route cache.  Returns `0` when the mtime is
/// unavailable.  Truncating the u128 nanos-since-epoch to `u64` preserves
/// every sub-second bit (the value only exceeds `u64` past the year ~2554,
/// saturated to `u64::MAX`); the fingerprint is only ever compared for
/// equality, so the absolute units never matter.
pub fn mtime_fingerprint(modified: Option<std::time::SystemTime>) -> u64 {
    modified.map_or(0, |t| {
        let nanos = t
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        u64::try_from(nanos).unwrap_or(u64::MAX)
    })
}

/// Mix a file's mtime fingerprint with its byte length into a single
/// equality-only cache fingerprint.
///
/// mtime alone misses a content edit that PRESERVES the modification time —
/// a timestamp-preserving checkout, `cp -p`, or build-cache restore can
/// rewrite a route file's contents while leaving its mtime untouched, which
/// would otherwise serve a STALE generated router / OpenAPI spec from the
/// cache. Folding in `len()` catches every such edit that changes the file
/// size (the overwhelming majority), at ZERO extra compile-time cost: the
/// metadata is already stat'd for the mtime and no file contents are ever
/// hashed. The fingerprint is only ever compared for equality, so any stable
/// mix works; this one is strictly more sensitive than mtime alone.
pub fn combine_fingerprint(mtime: u64, len: u64) -> u64 {
    mtime.rotate_left(1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ len.wrapping_mul(0xD1B5_4A32_D192_ED03)
}

/// Compile-time cache fingerprint for a source file from its already-fetched
/// [`std::fs::Metadata`] — combines mtime ([`mtime_fingerprint`]) and size
/// ([`combine_fingerprint`]).
///
/// This is the ONE place the mtime/size mixing is spelled out: every
/// compile-time cache that already holds a `Metadata` (route-file scanning
/// here, sidecar and macro-source fingerprinting in
/// `vespera_impl::cache`) goes through it, so a fingerprint can never
/// silently degrade back to mtime-only in one consumer while the others
/// guard against timestamp-preserving edits. Costs no syscall: `len()` is
/// materialised by the same `stat` as the mtime.
// `pub` (not `pub(crate)`) to match the rest of this private module — see
// `clippy::redundant_pub_crate`; visibility is still crate-internal because
// `mod file_utils` itself is private.
pub fn file_fingerprint(meta: &std::fs::Metadata) -> u64 {
    combine_fingerprint(mtime_fingerprint(meta.modified().ok()), meta.len())
}

fn collect_with_mtimes_into(folder_path: &Path, out: &mut Vec<(PathBuf, u64)>) -> io::Result<()> {
    for entry in std::fs::read_dir(folder_path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_file() {
            // Only `.rs` files feed route discovery and cache
            // fingerprinting — both consumers (`collect_metadata_from_files`
            // and `fingerprints_from_scan`) filter by extension — so skip
            // the `metadata()` stat for every other file (fixtures, JSON,
            // uploads, …).  On Unix that is one `stat` saved per non-Rust
            // file at compile time; the entry still keeps its place in the
            // list with mtime `0` (never read for non-`.rs` paths).
            // An unavailable `Metadata` keeps the `0` sentinel.
            let mtime = if path.extension().is_some_and(|e| e == "rs") {
                entry.metadata().as_ref().map_or(0, file_fingerprint)
            } else {
                0
            };
            out.push((path, mtime));
        } else if file_type.is_dir() {
            collect_with_mtimes_into(&path, out)?;
        }
    }
    Ok(())
}

pub fn file_to_segments(file: &Path, base_path: &Path) -> Vec<String> {
    let file_stem = file
        .strip_prefix(base_path)
        .map_or_else(|_| normalize_display_path(file), normalize_display_path);
    // Strip ONLY a trailing `.rs` extension (not every `.rs` substring): a
    // path component that legitimately contains `.rs` (e.g. a directory named
    // `v1.rs`) must keep it, so `replace(".rs", "")` — which mangled every
    // occurrence — is wrong.  Normalize `\` → `/` afterwards.
    let file_stem = file_stem
        .strip_suffix(".rs")
        .unwrap_or(&file_stem)
        .replace('\\', "/");
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

    #[test]
    fn mtime_fingerprint_distinguishes_subsecond_edits() {
        use std::time::{Duration, UNIX_EPOCH};

        // Two mtimes in the SAME wall-clock second, 1 ms apart (1 ms is
        // safely above the 100 ns `SystemTime`/FILETIME resolution on
        // Windows, so the delta is actually representable): the prior
        // seconds-only fingerprint collapsed these to one value (the
        // stale-cache bug); the nanosecond fingerprint MUST tell them apart
        // so a same-second edit always invalidates the route cache.
        let base = UNIX_EPOCH + Duration::new(1_700_000_000, 0);
        let same_second_later = base + Duration::from_millis(1);
        assert_ne!(
            mtime_fingerprint(Some(base)),
            mtime_fingerprint(Some(same_second_later)),
            "same-second edits must produce distinct cache fingerprints"
        );

        // A whole-second difference is of course still distinguished.
        let next_second = base + Duration::from_secs(1);
        assert_ne!(
            mtime_fingerprint(Some(base)),
            mtime_fingerprint(Some(next_second))
        );

        // Unavailable mtime collapses to 0 (unchanged contract).
        assert_eq!(mtime_fingerprint(None), 0);
    }

    #[test]
    fn combine_fingerprint_is_sensitive_to_mtime_and_size() {
        // Same mtime, DIFFERENT size — the timestamp-preserving content edit
        // the size term is here to catch — must produce distinct fingerprints.
        assert_ne!(
            combine_fingerprint(42, 100),
            combine_fingerprint(42, 101),
            "same mtime + different size must differ (stale-cache guard)"
        );
        // Different mtime, same size — still distinguished (mtime term).
        assert_ne!(combine_fingerprint(42, 100), combine_fingerprint(43, 100));
        // Identical (mtime, size) — equal (a genuine cache hit).
        assert_eq!(combine_fingerprint(42, 100), combine_fingerprint(42, 100));
    }

    #[test]
    fn normalized_path_helpers_cover_borrowed_and_separator_paths() {
        assert_eq!(normalize_display_path("routes/users.rs"), "routes/users.rs");
        assert!(paths_equal_normalized(
            Some("routes\\users.rs"),
            Some("routes/users.rs")
        ));
        assert!(!paths_equal_normalized(
            Some("routes/a.rs"),
            Some("routes/b.rs")
        ));
    }
}
