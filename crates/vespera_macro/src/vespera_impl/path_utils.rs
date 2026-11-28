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
    let base = match crate::env_access::var("CARGO_CRATE_NAME") {
        Ok(krate) if krate != package.replace('-', "_") => format!("{package}.{krate}"),
        _ => package,
    };
    format!("{base}{}", target_variant())
}

/// Suffix that keeps the build files of one crate's concurrently compiled
/// variants apart: `.bin` for a binary (cargo sets `CARGO_BIN_NAME` — a
/// binary named like its package has the library's crate name), then
/// [`variant_of`] and [`build_discriminator`].
///
/// The library, its `cargo test` harness build and rust-analyzer all expand
/// the same `vespera!` with possibly different `#[cfg(test)]` items; sharing
/// one cache and one content-addressed sidecar stem, each build pruned the
/// file the other's `include_str!` still pointed at.
pub(super) fn target_variant() -> String {
    let bin = if crate::env_access::var_os("CARGO_BIN_NAME").is_some() {
        ".bin"
    } else {
        ""
    };
    let args = crate::env_access::compiler_args();
    format!(
        "{bin}{}{}",
        variant_of(&args, std::env::current_exe().ok().as_deref()),
        build_discriminator(&args)
    )
}

/// `.<profile>` for a non-`debug` build profile (read off rustc's
/// `--out-dir <target>/[<triple>/]<profile>/{deps,examples}`), then
/// `.<8 hex>` of the enabled `--cfg feature="…"` set: alternating
/// `cargo build` / `cargo build --release` (or feature sets) otherwise shared
/// one route cache (never hitting) and one sidecar prune stem (each build
/// pruning the other's `include_str!` target).  `""` outside cargo.
fn build_discriminator(args: &[String]) -> String {
    use std::hash::{Hash, Hasher};
    let profile = out_dir_arg(args)
        .map(Path::new)
        .filter(|out_dir| {
            out_dir
                .file_name()
                .is_some_and(|kind| kind == "deps" || kind == "examples")
        })
        .and_then(|out_dir| out_dir.parent()?.file_name())
        .map(|profile| profile.to_string_lossy().into_owned())
        .filter(|profile| profile != "debug")
        .map_or_else(String::new, |profile| format!(".{profile}"));
    let mut features: Vec<&str> = args
        .windows(2)
        .filter(|pair| pair[0] == "--cfg" && pair[1].starts_with("feature="))
        .map(|pair| pair[1].as_str())
        .collect();
    if features.is_empty() {
        return profile;
    }
    features.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    features.hash(&mut hasher);
    format!("{profile}.{:08x}", hasher.finish() & 0xffff_ffff)
}

/// The value of rustc's `--out-dir <dir>` / `--out-dir=<dir>` (or
/// rustdoc's `-o <dir>`).
fn out_dir_arg(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find_map(|pair| matches!(pair[0].as_str(), "--out-dir" | "-o").then(|| pair[1].as_str()))
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--out-dir=")))
}

/// `.ide` inside rust-analyzer's proc-macro server (`exe`), `.test` for a
/// `rustc --test` harness build (the only trace of `cfg(test)` a proc macro
/// can observe), else `""`.
fn variant_of(args: &[String], exe: Option<&Path>) -> &'static str {
    if crate::macro_storage::is_rust_analyzer(exe) {
        ".ide"
    } else if args.iter().any(|arg| arg == "--test") {
        ".test"
    } else {
        ""
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
/// 1. `VESPERA_TARGET_DIR` (relative: against the workspace root) — an
///    explicit override, e.g. for builds outside cargo;
/// 2. the target directory of the running compilation, read off rustc's
///    `--out-dir <target>/<profile>/deps` (an absolute path only) — the only
///    answer that is right for every crate in the build: a registry / git
///    dependency has no workspace of its own (its `export_app!` spec would
///    land in its source directory, where the parent never looks);
/// 3. `CARGO_TARGET_DIR` (relative: joined onto the workspace root);
/// 4. `<workspace root>/target` (rust-analyzer and other non-cargo hosts).
pub fn find_target_dir(manifest_path: &Path) -> PathBuf {
    let workspace_root = || find_workspace_root(manifest_path);
    if let Some(dir) = crate::env_access::var_os("VESPERA_TARGET_DIR").filter(|dir| !dir.is_empty())
    {
        return workspace_root().join(dir);
    }
    compiler_target_dir(&crate::env_access::compiler_args()).unwrap_or_else(|| {
        target_dir_in(
            &workspace_root(),
            crate::env_access::var_os("CARGO_TARGET_DIR"),
        )
    })
}

/// `<target>` from an absolute rustc `--out-dir <target>/[<triple>/]<profile>/{deps,examples}`
/// (with a `--target` triple: `<target>/<triple>`, consistently for every
/// crate of that build), or rustdoc's `-o` / `--out-dir <target>/[<triple>/]doc`
/// (`cargo doc`: the dependencies it documents against are built into the
/// same `<target>`).  `None` for any other output directory.
fn compiler_target_dir(args: &[String]) -> Option<PathBuf> {
    let out_dir = Path::new(out_dir_arg(args)?);
    let kind = out_dir.file_name()?;
    let target = if kind == "doc" {
        out_dir.parent()
    } else if kind == "deps" || kind == "examples" {
        out_dir.parent().and_then(Path::parent)
    } else {
        None
    };
    target
        .filter(|target| target.is_absolute())
        .map(Path::to_path_buf)
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
    let mut open = None;
    for raw in cargo_toml.lines() {
        let code = code_of_line(raw, &mut open);
        let line = code.split('#').next().unwrap_or_default().trim();
        if let Some(header) = line.strip_prefix('[') {
            let name = header.split(']').next().unwrap_or_default();
            if top_level_key_is_workspace(name) {
                return true;
            }
            top_level = false;
        } else if top_level
            && line
                .split_once('=')
                .is_some_and(|(key, _)| top_level_key_is_workspace(key))
        {
            return true;
        }
    }
    false
}

/// Whether a top-level key names the `workspace` table: `workspace`, a dotted
/// key under it (`workspace.members`, `workspace . members`), or a quoted
/// `"workspace"`. `workspace-foo` and `"workspace.x"` are other keys.
fn top_level_key_is_workspace(key: &str) -> bool {
    let first = key.trim().split('.').next().unwrap_or_default().trim();
    let unquoted = first
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            first
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(first);
    unquoted == "workspace"
}

/// The code of one TOML line with multi-line string contents (`"""` / `'''`)
/// removed: a `[workspace]` line inside such a string is text, not a table.
/// `open` carries an unterminated delimiter across lines. A delimiter opens
/// nothing inside a single-line string (`"..."` / `'...'`) or after a `#` (a
/// comment): `description = "don't '''"` does not open a string.
fn code_of_line(line: &str, open: &mut Option<&'static str>) -> String {
    const MULTI_LINE: [&str; 2] = ["\"\"\"", "'''"];
    let mut code = String::new();
    let mut rest = line;
    loop {
        if let Some(delim) = *open {
            let Some(end) = rest.find(delim) else {
                return code;
            };
            *open = None;
            rest = &rest[end + 3..];
            continue;
        }
        let Some(at) = rest.find(['#', '"', '\'']) else {
            code.push_str(rest);
            return code;
        };
        code.push_str(&rest[..at]);
        let tail = &rest[at..];
        if tail.starts_with('#') {
            return code;
        }
        if let Some(delim) = MULTI_LINE
            .into_iter()
            .find(|delim| tail.starts_with(*delim))
        {
            *open = Some(delim);
            rest = &tail[3..];
            continue;
        }
        let after = after_single_line_string(tail);
        code.push_str(&tail[..tail.len() - after.len()]);
        rest = after;
    }
}

/// The text after the single-line string at the start of `text`: a `"..."`
/// basic string (a `\` escapes the next character) or a `'...'` literal. An
/// unterminated string runs to the end of the line.
fn after_single_line_string(text: &str) -> &str {
    let basic = text.starts_with('"');
    let close = if basic { '"' } else { '\'' };
    let mut chars = text.char_indices().skip(1);
    while let Some((index, ch)) = chars.next() {
        if ch == close {
            return &text[index + ch.len_utf8()..];
        }
        if basic && ch == '\\' {
            chars.next();
        }
    }
    ""
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
    fn test_find_workspace_root_uses_the_workspace_manifest_dir() {
        // The temp dir is its own workspace root: an ancestor of the temp dir
        // (a checkout with a Cargo.lock, a parent `[workspace]`) must not
        // decide the answer, so the root marker is written here.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[workspace]
",
        )
        .expect("write workspace manifest");
        let manifest_path = temp_dir.path();
        let result = find_workspace_root(manifest_path);
        assert_eq!(result, manifest_path);
    }

    #[test]
    fn test_find_workspace_root_with_cargo_lock() {
        // Test finding target dir with Cargo.lock present
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let manifest_path = temp_dir.path();

        // Its own workspace root, so an ancestor Cargo.lock cannot decide it.
        fs::write(manifest_path.join("Cargo.toml"), "[workspace]\n")
            .expect("write workspace manifest");
        fs::write(manifest_path.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");

        let result = find_workspace_root(manifest_path);
        assert_eq!(result, manifest_path);
    }

    #[test]
    fn workspace_fallback_uses_the_topmost_cargo_lock_dir() {
        // No `[workspace]` anywhere: the outermost directory holding a
        // Cargo.lock wins over the nearer one.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let top = temp_dir.path();
        let member = top.join("member");
        fs::create_dir(&member).expect("Failed to create member dir");
        fs::write(top.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");
        fs::write(member.join("Cargo.lock"), "").expect("Failed to write Cargo.lock");
        fs::write(member.join("Cargo.toml"), "[package]\nname = \"member\"")
            .expect("Failed to write Cargo.toml");
        assert_eq!(find_workspace_root(&member), top);
    }

    #[test]
    fn workspace_fallback_without_lock_or_workspace_is_the_crate() {
        // No `[workspace]` and no Cargo.lock on the way up: the crate itself.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let crate_dir = temp_dir.path().join("crate");
        fs::create_dir(&crate_dir).expect("Failed to create crate dir");
        fs::write(crate_dir.join("Cargo.toml"), "[package]\nname = \"crate\"")
            .expect("Failed to write Cargo.toml");
        assert_eq!(find_workspace_root(&crate_dir), crate_dir);
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
    #[case::quoted_table("[\"workspace\"]\nmembers = []", true)]
    #[case::quoted_dotted_table_is_one_key("[\"workspace.x\"]", false)]
    #[case::trailing_comment("[workspace] # root", true)]
    #[case::inline_key("workspace = { members = [] }\n[package]", true)]
    #[case::dotted_key("workspace.members = []\n[package]", true)]
    #[case::spaced_dotted_key("workspace . members = []", true)]
    #[case::quoted_key("\"workspace\" = { members = [] }", true)]
    #[case::hyphenated_key("workspace-foo = 1\n[package]", false)]
    #[case::quoted_dotted_key_is_one_key("\"workspace.x\" = 1", false)]
    #[case::member_pointer("[package]\nname = \"a\"\nworkspace = \"..\"", false)]
    #[case::commented_out("# [workspace]\n[package]", false)]
    #[case::string_value("[package]\ndescription = \"[workspace]\"", false)]
    #[case::other_table("[package.workspace]", false)]
    #[case::array_table("[[bin]]\nname = \"x\"", false)]
    #[case::multiline_string_body("[package]\ndescription = \"\"\"\n[workspace]\n\"\"\"", false)]
    #[case::literal_multiline_body("[package]\nd = '''\n[workspace]\n'''", false)]
    #[case::table_after_multiline_string("d = \"\"\"\nx\n\"\"\"\n[workspace]", true)]
    #[case::one_line_multiline_string("d = \"\"\"x\"\"\"\n[workspace]", true)]
    #[case::delimiter_in_comment("# \"\"\" not a string\n[workspace]", true)]
    #[case::delimiter_in_basic_string("[package]\ndescription = \"don't '''\"\n[workspace]", true)]
    #[case::delimiter_in_literal_string("[package]\nd = '\"\"\"'\n[workspace]", true)]
    #[case::escaped_quote_in_basic_string("d = \"a \\\" '''\"\n[workspace]", true)]
    #[case::unterminated_basic_string_is_one_line("d = \"x '''\n[workspace]", true)]
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
        let _vespera = crate::env_access::test_env::remove_var("VESPERA_TARGET_DIR");
        let _cargo = crate::env_access::test_env::remove_var("CARGO_TARGET_DIR");
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        // A workspace root of its own: the answer does not depend on ancestors.
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[workspace]
",
        )
        .expect("write workspace manifest");
        assert_eq!(
            find_target_dir(temp_dir.path()),
            temp_dir.path().join("target")
        );
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

        // An absolute folder is returned unchanged, not joined under `src/`.
        let result = find_folder_path(&absolute_path).unwrap();
        assert_eq!(result, PathBuf::from(&absolute_path));
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
        assert_eq!(result, temp_dir.path().join("src").join("routes"));
    }

    /// A binary named differently from its package gets its own tag; the
    /// library (and its test build) keep the package name.
    #[rstest::rstest]
    #[case::library(Some("my_app"), None, "", "my-app")]
    #[case::binary(Some("server"), Some("server"), "", "my-app.server.bin")]
    #[case::binary_named_like_the_package(Some("my_app"), Some("my-app"), "", "my-app.bin")]
    #[case::library_test_harness(Some("my_app"), None, "rustc\n--test", "my-app.test")]
    #[case::binary_test_harness(Some("my_app"), Some("my-app"), "rustc\n--test", "my-app.bin.test")]
    #[case::outside_cargo(None, None, "", "my-app")]
    fn crate_tag_separates_binaries_and_test_builds(
        #[case] krate: Option<&str>,
        #[case] bin: Option<&str>,
        #[case] args: &str,
        #[case] expected: &str,
    ) {
        use crate::env_access::test_env::{remove_var, set_var};
        let _pkg = set_var("CARGO_PKG_NAME", "my-app");
        let _krate = krate.map_or_else(
            || remove_var("CARGO_CRATE_NAME"),
            |krate| set_var("CARGO_CRATE_NAME", krate),
        );
        let _bin = bin.map_or_else(
            || remove_var("CARGO_BIN_NAME"),
            |bin| set_var("CARGO_BIN_NAME", bin),
        );
        let _args = set_var(crate::env_access::COMPILER_ARGS_KEY, args);
        assert_eq!(current_crate_tag(), expected);
    }

    /// Release builds and feature sets get their own build files; a debug
    /// build without features keeps the plain tag.
    #[rstest::rstest]
    #[case::outside_cargo(&[], "")]
    #[case::debug(&["--out-dir", "/t/debug/deps"], "")]
    #[case::release(&["--out-dir=/t/release/deps"], ".release")]
    #[case::cross_release(&["--out-dir", "/t/x86_64-unknown-linux-gnu/release/examples"], ".release")]
    #[case::rustdoc(&["-o", "/t/doc"], "")]
    #[case::shallow(&["--out-dir", "deps"], "")]
    fn build_discriminator_separates_profiles(#[case] args: &[&str], #[case] expected: &str) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        assert_eq!(build_discriminator(&args), expected);
    }

    #[test]
    fn build_discriminator_hashes_the_feature_set() {
        let args =
            |list: &[&str]| -> Vec<String> { list.iter().map(|arg| (*arg).to_owned()).collect() };
        let ab = build_discriminator(&args(&["--cfg", "feature=\"a\"", "--cfg", "feature=\"b\""]));
        let ba = build_discriminator(&args(&[
            "--cfg",
            "feature=\"b\"",
            "--cfg",
            "feature=\"a\"",
            "--cfg",
            "unix",
        ]));
        let a = build_discriminator(&args(&[
            "--out-dir",
            "/t/release/deps",
            "--cfg",
            "feature=\"a\"",
        ]));
        assert_eq!(ab, ba, "order and non-feature cfgs do not matter");
        assert_eq!(ab.len(), 9, "{ab}");
        assert!(
            a.starts_with(".release.") && a != format!(".release{ab}"),
            "{a}"
        );
    }

    #[rstest::rstest]
    #[case::plain_rustc(&["rustc"], Some("rustc"), "")]
    #[case::test_harness(&["rustc", "--test"], Some("rustc"), ".test")]
    #[case::rust_analyzer(&["--test"], Some("rust-analyzer-proc-macro-srv"), ".ide")]
    #[case::rust_analyzer_exe(&[], Some("rust-analyzer.exe"), ".ide")]
    #[case::unknown_exe(&[], None, "")]
    fn build_variant_tells_test_and_ide_builds_apart(
        #[case] args: &[&str],
        #[case] exe: Option<&str>,
        #[case] expected: &str,
    ) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let exe = exe.map(|name| Path::new("/toolchain/bin").join(name));
        assert_eq!(variant_of(&args, exe.as_deref()), expected);
    }

    fn absolute(path: &str) -> PathBuf {
        std::env::temp_dir().join(path)
    }

    #[rstest::rstest]
    #[case::deps(&["--out-dir", "{t}/debug/deps"], Some("{t}"))]
    #[case::cross_target(&["--out-dir", "{t}/x86_64-unknown-linux-gnu/release/deps"], Some("{t}/x86_64-unknown-linux-gnu"))]
    #[case::examples(&["--out-dir={t}/debug/examples"], Some("{t}"))]
    #[case::rustdoc_output(&["-o", "{t}/doc"], Some("{t}"))]
    #[case::rustdoc_cross_target(&["--out-dir", "{t}/x86_64-unknown-linux-gnu/doc"], Some("{t}/x86_64-unknown-linux-gnu"))]
    #[case::rustc_output_file(&["-o", "{t}/debug/deps/app"], None)]
    #[case::relative_doc(&["-o", "target/doc"], None)]
    #[case::relative(&["--out-dir", "target/debug/deps"], None)]
    #[case::shallow(&["--out-dir", "/deps"], None)]
    #[case::missing_value(&["--out-dir"], None)]
    #[case::absent(&["rustc"], None)]
    fn target_dir_is_read_off_the_out_dir(#[case] args: &[&str], #[case] expected: Option<&str>) {
        let target = absolute("vespera-target").display().to_string();
        let args: Vec<String> = args.iter().map(|arg| arg.replace("{t}", &target)).collect();
        let expected = expected.map(|path| PathBuf::from(path.replace("{t}", &target)));
        assert_eq!(compiler_target_dir(&args), expected);
    }

    /// `VESPERA_TARGET_DIR` beats the compiler's `--out-dir`, which beats
    /// `CARGO_TARGET_DIR`.
    #[test]
    fn target_dir_precedence() {
        use crate::env_access::test_env::{remove_var, set_var};
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();
        // Its own workspace root, so an ancestor Cargo.lock cannot decide it.
        fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("write workspace manifest");
        let built = absolute("vespera-built");
        let _cargo = set_var("CARGO_TARGET_DIR", "cargo-target");
        let _args = set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            format!(
                "rustc\n--out-dir\n{}",
                built.join("debug").join("deps").display()
            ),
        );
        {
            let _vespera = set_var("VESPERA_TARGET_DIR", "custom");
            assert_eq!(find_target_dir(root), root.join("custom"));
        }
        let _vespera = set_var("VESPERA_TARGET_DIR", "");
        assert_eq!(find_target_dir(root), built);
        let _args = remove_var(crate::env_access::COMPILER_ARGS_KEY);
        assert_eq!(find_target_dir(root), root.join("cargo-target"));
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
