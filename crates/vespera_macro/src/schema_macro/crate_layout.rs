//! Where the crate being compiled keeps its modules.
//!
//! `crate::` paths resolve from the crate root of the target being compiled,
//! which is not always `src/lib.rs`: an integration test's root is
//! `tests/<name>.rs` (so `tests/common/mod.rs` is `crate::common`), an
//! example's `examples/<name>.rs`, a bench's `benches/<name>.rs`, an extra
//! binary's `src/bin/<name>.rs`. The layout is derived from the macro
//! call-site file relative to `CARGO_MANIFEST_DIR` (never from an ancestor
//! directory that happens to be named `src`), with `CARGO_CRATE_NAME` /
//! `CARGO_BIN_NAME` picking the root of a multi-file target.

use std::path::{Path, PathBuf};

use super::file_cache;

/// The module directory of the crate root and its root file candidates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrateLayout {
    /// Directory the crate root's child modules live in (`src`, `tests`,
    /// `examples/tool`, ...).
    pub module_dir: PathBuf,
    /// Crate root file candidates, most likely first.
    pub roots: Vec<PathBuf>,
}

impl CrateLayout {
    /// Whether `file` belongs to the crate (a root or below the module
    /// directory).
    pub fn contains(&self, file: &Path) -> bool {
        self.roots.iter().any(|root| same_path(root, file))
            || relative_to(file, &self.module_dir).is_some()
    }

    /// The absolute module path (`["crate", ..]`) of the source `file`:
    /// a crate root is `crate`; a file the module tree loads through a
    /// `#[path = ".."]` declaration is the module declaring it
    /// ([`super::module_scope::declared_module_of`]); any other file is
    /// named by its path below [`Self::module_dir`] (`a/b.rs` / `a/b/mod.rs`
    /// -> `crate::a::b`). A file outside the module directory the tree does
    /// not load maps to `crate`.
    pub fn module_path_of(&self, file: &Path) -> Vec<String> {
        if self.roots.iter().any(|root| same_path(root, file)) {
            return vec!["crate".to_string()];
        }
        let disk = self.disk_module_path_of(file);
        super::module_scope::declared_module_of(self, file, &disk).unwrap_or(disk)
    }

    /// [`Self::module_path_of`] from the file's place on disk alone.
    fn disk_module_path_of(&self, file: &Path) -> Vec<String> {
        let mut segments = vec!["crate".to_string()];
        let Some(relative) = relative_to(file, &self.module_dir) else {
            return segments;
        };
        let names: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
        // The default crate roots and every `src/bin/` binary, when the
        // layout is the crate's `src`. A `bin/` directory of a test,
        // example or bench target is ordinary module code (`tests/bin/mod.rs`
        // is `crate::bin`), so the rule only applies to `src`.
        // Only a test, example or bench target directory (or one below it)
        // is excluded: the crate's `src` and a plain layout dir keep the rule.
        // The parent counts only for a target's sub-layout (`tests/multi-part`):
        // a library's `src` lives in the manifest directory, whose own name
        // (a crate checked out as `tests/`) says nothing about the target.
        let in_target = |dir: Option<&std::ffi::OsStr>| {
            dir.is_some_and(|name| matches!(name.to_str(), Some("tests" | "examples" | "benches")))
        };
        let library_src = self.module_dir.file_name() == Some(std::ffi::OsStr::new("src"));
        let root_rules = !in_target(self.module_dir.file_name())
            && (library_src || !in_target(self.module_dir.parent().and_then(Path::file_name)));
        // A `src/bin/` file is a binary's root only in the build of that binary
        // (`CARGO_BIN_NAME`): in a library it is a module when `mod bin;`
        // declares it, so its module path is `crate::bin::..` (R27-SM1-L3).
        let bin_target = crate_var("CARGO_BIN_NAME");
        let is_bin = |target: &str| bin_target.as_deref() == Some(target);
        let is_root = root_rules
            && match names.as_slice() {
                [file] => *file == "main.rs" || *file == "lib.rs",
                [bin, file] => {
                    let path = Path::new(file);
                    *bin == "bin"
                        && path.extension().is_some_and(|ext| ext == "rs")
                        && path
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .is_some_and(is_bin)
                }
                [bin, dir, file] => *bin == "bin" && *file == "main.rs" && is_bin(dir),
                _ => false,
            };
        if is_root {
            return segments;
        }
        for name in names {
            match name.strip_suffix(".rs") {
                Some("mod") => {}
                Some(stem) => segments.push(stem.to_string()),
                None => segments.push(name.to_string()),
            }
        }
        segments
    }
}

/// The layout of the crate the current macro invocation compiles.
///
/// Memoised per macro invocation (one `schema_type!` asks ~100 times; each
/// computation reads the current directory and probes target roots): the
/// memo is keyed by the epoch and by every input of [`layout_for`].
pub fn crate_layout() -> Option<CrateLayout> {
    type Key = (u64, String, Option<PathBuf>, Option<String>, Option<String>);
    thread_local! {
        static MEMO: std::cell::RefCell<Option<(Key, CrateLayout)>> =
            const { std::cell::RefCell::new(None) };
    }
    let manifest_dir = file_cache::get_manifest_dir()?;
    let call_site = proc_macro2::Span::call_site().local_file();
    let key: Key = (
        file_cache::current_epoch(),
        manifest_dir,
        call_site,
        crate_var("CARGO_BIN_NAME"),
        crate_var("CARGO_CRATE_NAME"),
    );
    let hit = MEMO.with(|memo| {
        memo.borrow()
            .as_ref()
            .filter(|(memo_key, _)| *memo_key == key)
            .map(|(_, layout)| layout.clone())
    });
    if let Some(layout) = hit {
        return Some(layout);
    }
    let layout = layout_for(Path::new(&key.1), key.2.as_deref());
    MEMO.with(|memo| *memo.borrow_mut() = Some((key, layout.clone())));
    Some(layout)
}

/// The layout for a crate in `manifest_dir` whose macro call site is
/// `call_site` (`None`: the library / main binary layout).
pub fn layout_for(manifest_dir: &Path, call_site: Option<&Path>) -> CrateLayout {
    let src = manifest_dir.join("src");
    let relative = call_site.and_then(|file| relative_to(file, manifest_dir));
    let parts: Vec<&str> = relative
        .as_deref()
        .map(|relative| relative.split('/').collect())
        .unwrap_or_default();
    let target = match parts.as_slice() {
        [kind @ ("tests" | "examples" | "benches"), entry, ..] => Some((
            manifest_dir.join(kind),
            crate_var("CARGO_CRATE_NAME"),
            *entry,
        )),
        // Only in a binary's build: a library's `src/bin/` file is its module tree.
        ["src", "bin", entry, ..] if crate_var("CARGO_BIN_NAME").is_some() => {
            Some((src.join("bin"), crate_var("CARGO_BIN_NAME"), *entry))
        }
        _ => None,
    };
    let custom_target = target.is_none();
    if let Some((dir, target_name, entry)) = target
        && let Some(root) = target_root(&dir, target_name.as_deref(), entry)
    {
        let module_dir = root.parent().map_or_else(|| dir.clone(), Path::to_path_buf);
        return CrateLayout {
            module_dir,
            roots: vec![root],
        };
    }
    // A custom `[[test]]` / `[[bench]]` / `[[example]]` / `[[bin]]` `path` outside
    // the default directories (R28-SM1-L1) is rooted at the file rustc compiles.
    if custom_target
        && let Some(call_site) = call_site
        && relative_to(call_site, &src).is_none()
        && let Some(layout) = compiler_root_layout(call_site)
    {
        return layout;
    }
    let (lib, main) = (src.join("lib.rs"), src.join("main.rs"));
    let roots = if crate::env_access::var("CARGO_BIN_NAME").is_ok() {
        vec![main, lib]
    } else {
        vec![lib, main]
    };
    CrateLayout {
        module_dir: src,
        roots,
    }
}

/// The layout rooted at the crate root rustc compiles (its positional `.rs`
/// input), when that root's directory holds the call site: the same rule
/// `vespera_impl::cron_discovery` uses for `vespera!` call sites.
fn compiler_root_layout(call_site: &Path) -> Option<CrateLayout> {
    let root = crate::env_access::compiler_args()
        .iter()
        .map(PathBuf::from)
        .find(|arg| arg.extension().is_some_and(|ext| ext == "rs") && arg.is_file())?;
    let module_dir = root.parent()?.to_path_buf();
    relative_to(call_site, &module_dir)?;
    Some(CrateLayout {
        module_dir,
        roots: vec![root],
    })
}

fn crate_var(name: &str) -> Option<String> {
    crate::env_access::var(name).ok()
}

/// The root file of the target in `dir` (`tests/`, `src/bin/`, ...): the
/// one named after the target (`<name>.rs` / `<name>/main.rs`, also with
/// `_` spelled `-`), else the entry the call site sits in.
fn target_root(dir: &Path, target_name: Option<&str>, entry: &str) -> Option<PathBuf> {
    let names = target_name
        .into_iter()
        .flat_map(|name| [name.to_string(), name.replace('_', "-")])
        .chain(std::iter::once(
            entry.strip_suffix(".rs").unwrap_or(entry).to_string(),
        ));
    names
        .flat_map(|name| {
            [
                dir.join(format!("{name}.rs")),
                dir.join(name).join("main.rs"),
            ]
        })
        .find(|candidate| candidate.is_file())
}

/// `file` below `dir` as a `/`-separated relative path (both sides made
/// absolute against the current directory — rustc's, which the
/// cwd-relative span paths are relative to — and compared like
/// [`crate::file_utils::normalize_path_key`]). The segments keep the
/// file's own spelling: the key is lowercased on Windows, which turned
/// `src/models/UserDto.rs` into `crate::models::userdto` there only.
fn relative_to(file: &Path, dir: &Path) -> Option<String> {
    let key = path_key(file);
    let relative = key.strip_prefix(&path_key(dir))?.strip_prefix('/')?;
    let count = relative.split('/').count();
    // The file's components with `.` / `..` folded, like the key.
    let mut names: Vec<String> = Vec::new();
    for component in file.components() {
        match component {
            std::path::Component::Normal(name) => names.push(name.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => {
                names.pop();
            }
            _ => {}
        }
    }
    let skip = names.len().checked_sub(count)?;
    Some(names[skip..].join("/"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

pub(super) fn path_key(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::file_utils::normalize_path_key(&path.to_string_lossy(), &cwd)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    const FILES: &[&str] = &[
        "src/lib.rs",
        "src/main.rs",
        "src/bin/tool.rs",
        "src/bin/multi/main.rs",
        "src/bin/multi/helper.rs",
        "tests/api.rs",
        "tests/common/mod.rs",
        "tests/bin/mod.rs",
        "tests/multi_part/main.rs",
        "tests/multi_part/util.rs",
        "examples/demo.rs",
        "benches/speed.rs",
    ];

    fn temp_crate() -> TempDir {
        let dir = TempDir::new().unwrap();
        for file in FILES {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    /// `(call site, CARGO_CRATE_NAME, CARGO_BIN_NAME)` -> module dir, root,
    /// and the call-site file's module path.
    #[rstest]
    #[case::library("src/lib.rs", None, None, "src", "src/lib.rs", "crate")]
    #[case::library_module("src/a/b.rs", None, None, "src", "src/lib.rs", "crate::a::b")]
    #[case::nested_module("src/a/b/c/d.rs", None, None, "src", "src/lib.rs", "crate::a::b::c::d")]
    #[case::main_binary("src/x.rs", None, Some("app"), "src", "src/main.rs", "crate::x")]
    #[case::integration_test("tests/api.rs", Some("api"), None, "tests", "tests/api.rs", "crate")]
    #[case::test_helper(
        "tests/common/mod.rs",
        Some("api"),
        None,
        "tests",
        "tests/api.rs",
        "crate::common"
    )]
    // A `bin/` directory under `tests/` is a helper module, not a binary root.
    #[case::test_bin_helper(
        "tests/bin/mod.rs",
        Some("api"),
        None,
        "tests",
        "tests/api.rs",
        "crate::bin"
    )]
    #[case::multi_file_test(
        "tests/multi_part/util.rs",
        Some("multi_part"),
        None,
        "tests/multi-part",
        "tests/multi-part/main.rs",
        "crate::util"
    )]
    #[case::test_without_crate_name("tests/api.rs", None, None, "tests", "tests/api.rs", "crate")]
    #[case::example(
        "examples/demo.rs",
        Some("demo"),
        None,
        "examples",
        "examples/demo.rs",
        "crate"
    )]
    #[case::bench(
        "benches/speed.rs",
        Some("speed"),
        None,
        "benches",
        "benches/speed.rs",
        "crate"
    )]
    #[case::bin_file(
        "src/bin/tool.rs",
        None,
        Some("tool"),
        "src/bin",
        "src/bin/tool.rs",
        "crate"
    )]
    // A library's `src/bin/` file is its module (`mod bin;`), not a binary root
    // (R27-SM1-L3): no `CARGO_BIN_NAME` is set while the library compiles.
    #[case::library_bin_module(
        "src/bin/tool.rs",
        None,
        None,
        "src",
        "src/lib.rs",
        "crate::bin::tool"
    )]
    #[case::bin_dir(
        "src/bin/multi/helper.rs",
        None,
        Some("multi"),
        "src/bin/multi",
        "src/bin/multi/main.rs",
        "crate::helper"
    )]
    // No root on disk for the entry: the library layout.
    #[case::unknown_test("tests/gone.rs", Some("gone"), None, "src", "src/lib.rs", "crate")]
    fn layouts(
        #[case] call_site: &str,
        #[case] crate_name: Option<&str>,
        #[case] bin_name: Option<&str>,
        #[case] module_dir: &str,
        #[case] root: &str,
        #[case] module: &str,
    ) {
        let dir = temp_crate();
        // `multi_part` is spelled with a dash on disk.
        std::fs::rename(
            dir.path().join("tests/multi_part"),
            dir.path().join("tests/multi-part"),
        )
        .unwrap();
        let call_site = call_site.replace("multi_part", "multi-part");
        let set = |name: &str, value: Option<&str>| {
            value.map_or_else(
                || crate::env_access::test_env::remove_var(name),
                |value| crate::env_access::test_env::set_var(name, value),
            )
        };
        let _crate = set("CARGO_CRATE_NAME", crate_name);
        let _bin = set("CARGO_BIN_NAME", bin_name);
        let file = dir.path().join(&call_site);
        let layout = layout_for(dir.path(), Some(&file));
        assert!(
            same_path(&layout.module_dir, &dir.path().join(module_dir)),
            "{layout:?}"
        );
        assert!(
            same_path(&layout.roots[0], &dir.path().join(root)),
            "{layout:?}"
        );
        assert_eq!(layout.module_path_of(&file).join("::"), module);
    }

    /// An ancestor directory named `src` (`/home/me/src/app/tests/..`) does
    /// not root the crate: only the manifest directory does.
    #[test]
    fn ancestor_src_directory_is_ignored() {
        let outer = TempDir::new().unwrap();
        let manifest = outer.path().join("src").join("app");
        std::fs::create_dir_all(manifest.join("tests")).unwrap();
        std::fs::write(manifest.join("tests/api.rs"), "").unwrap();
        let _crate = crate::env_access::test_env::set_var("CARGO_CRATE_NAME", "api");
        let file = manifest.join("tests/api.rs");
        let layout = layout_for(&manifest, Some(&file));
        assert_eq!(layout.module_path_of(&file), ["crate"]);
        // A file outside the module directory maps to the crate root.
        assert_eq!(layout.module_path_of(&outer.path().join("x.rs")), ["crate"]);
        // No call site: the library layout.
        assert!(same_path(
            &layout_for(&manifest, None).module_dir,
            &manifest.join("src")
        ));
    }

    /// A crate checked out in a directory named like a target (`tests/`) keeps
    /// its binary roots: the manifest's own name decides nothing (R24-SM1-L1).
    /// The root is the binary's own build (`CARGO_BIN_NAME`, R27-SM1-L3).
    #[test]
    fn crate_named_like_a_target_keeps_its_bin_roots() {
        let outer = TempDir::new().unwrap();
        let manifest = outer.path().join("tests");
        std::fs::create_dir_all(manifest.join("src/bin")).unwrap();
        std::fs::write(manifest.join("src/lib.rs"), "").unwrap();
        let other = manifest.join("src/bin/other.rs");
        std::fs::write(&other, "").unwrap();
        let _bin = crate::env_access::test_env::set_var("CARGO_BIN_NAME", "other");
        let layout = layout_for(&manifest, None);
        assert_eq!(layout.module_path_of(&other), ["crate"]);
    }

    /// R28-SM1-L1: a custom `[[test]] path = "suite/main.rs"` (outside every
    /// default target directory) is rooted at the file rustc compiles, so its
    /// `crate::` modules resolve from `suite/`, not from the library `src/`.
    /// A call site outside that root's directory keeps the library layout.
    #[rstest]
    #[case::module_of_custom_root("suite/models/user.rs", "suite", "suite/main.rs")]
    #[case::outside_custom_root("tools/cli.rs", "src", "src/lib.rs")]
    fn custom_target_root_outside_default_dirs(
        #[case] call_site: &str,
        #[case] module_dir: &str,
        #[case] root: &str,
    ) {
        let dir = temp_crate();
        std::fs::create_dir_all(dir.path().join("suite/models")).unwrap();
        std::fs::create_dir_all(dir.path().join("tools")).unwrap();
        std::fs::write(dir.path().join("suite/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("suite/models/user.rs"), "").unwrap();
        std::fs::write(dir.path().join("tools/cli.rs"), "").unwrap();
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            format!("rustc\n{}", dir.path().join("suite/main.rs").display()),
        );
        let file = dir.path().join(call_site);
        let layout = layout_for(dir.path(), Some(&file));
        assert!(
            same_path(&layout.module_dir, &dir.path().join(module_dir)),
            "{layout:?}"
        );
        assert!(
            same_path(&layout.roots[0], &dir.path().join(root)),
            "{layout:?}"
        );
    }

    /// The module path of a custom root's child module (`crate::models::user`).
    #[test]
    fn custom_target_root_child_module_path() {
        let dir = temp_crate();
        std::fs::create_dir_all(dir.path().join("suite/models")).unwrap();
        std::fs::write(dir.path().join("suite/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("suite/models/user.rs"), "").unwrap();
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            format!("rustc\n{}", dir.path().join("suite/main.rs").display()),
        );
        let file = dir.path().join("suite/models/user.rs");
        let layout = layout_for(dir.path(), Some(&file));
        assert_eq!(
            layout.module_path_of(&file).join("::"),
            "crate::models::user"
        );
        assert_eq!(
            layout
                .module_path_of(&dir.path().join("suite/main.rs"))
                .join("::"),
            "crate"
        );
    }

    /// A cwd-relative call-site path (what rustc reports for workspace
    /// members) resolves like the absolute one.
    #[test]
    fn relative_call_site() {
        let cwd = std::env::current_dir().unwrap();
        let layout = layout_for(&cwd, Some(Path::new("src/a.rs")));
        assert_eq!(layout.module_path_of(Path::new("src/a.rs")), ["crate", "a"]);
        assert_eq!(
            layout.module_path_of(Path::new("src/x/../b.rs")),
            ["crate", "b"]
        );
    }

    /// M3 (round 16): a file loaded through `#[path]` is the module that
    /// declares it, as the forward lookup reads it (its place on disk gave
    /// `crate::generated::models`, and `crate` outside `src`).
    #[rstest]
    #[case::path_attr("src/generated/models.rs", "crate::models")]
    #[case::outside_src("gen/other.rs", "crate::other")]
    #[case::inline_parent("src/inner/deep.rs", "crate::inner::deep")]
    #[case::plain("src/plain.rs", "crate::plain")]
    #[case::below_path_module("src/generated/child.rs", "crate::models::child")]
    #[case::undeclared("src/stray.rs", "crate::stray")]
    #[case::missing("src/gone.rs", "crate::gone")]
    fn path_attr_modules(#[case] file: &str, #[case] module: &str) {
        let dir = TempDir::new().unwrap();
        let files = [
            (
                "src/lib.rs",
                "#[path = \"generated/models.rs\"] pub mod models; \
                 #[path = \"../gen/other.rs\"] pub mod other; pub mod plain; \
                 mod inner { #[path = \"deep.rs\"] pub mod deep; } mod absent;",
            ),
            ("src/generated/models.rs", "pub mod child;"),
            ("src/generated/child.rs", ""),
            ("gen/other.rs", ""),
            ("src/inner/deep.rs", ""),
            // A `#[path]` cycle back to itself ends.
            ("src/plain.rs", "#[path = \"plain.rs\"] mod again;"),
            ("src/stray.rs", ""),
        ];
        for (path, content) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        super::file_cache::bump_epoch();
        let layout = layout_for(dir.path(), None);
        let path = dir.path().join(file);
        assert_eq!(layout.module_path_of(&path).join("::"), module);
        // Memoised within the epoch.
        assert_eq!(layout.module_path_of(&path).join("::"), module);
    }

    /// L3 (round 13): module segments keep the file's spelling; the
    /// comparison key is lowercased on Windows (`mod UserDto;` used to give
    /// `crate::models::userdto` there).
    #[test]
    fn module_segments_keep_their_case() {
        let dir = TempDir::new().unwrap();
        let layout = layout_for(dir.path(), None);
        let file = dir.path().join("src").join("models").join("UserDto.rs");
        assert_eq!(layout.module_path_of(&file), ["crate", "models", "UserDto"]);
    }
}
