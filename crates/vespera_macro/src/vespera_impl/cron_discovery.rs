//! `#[cron]` job discovery for `vespera!`.
//!
//! Jobs come from two sources, mirroring how routes combine `ROUTE_STORAGE`
//! with a file scan:
//!
//! 1. **`CRON_STORAGE`** — every `#[cron]` attribute that rustc expanded
//!    *before* the `vespera!` invocation registered itself there.
//! 2. **Module-tree scan** — a `#[cron]` in a module that rustc expands
//!    *after* `vespera!` (declared later in the crate root, nested deeper, …)
//!    is not in the storage yet.  Instead of silently dropping it, the crate's
//!    module tree is walked from its root (`src/lib.rs` / `src/main.rs`)
//!    following `mod` declarations, and every `pub async fn` carrying
//!    `#[cron("…")]` / `#[vespera::cron("…")]` that the storage does not know
//!    yet is added.  Known jobs are matched per function, so a job declared
//!    after `vespera!` in a file whose earlier jobs are stored is found too.
//!
//! The same module tree places the stored jobs: a file path alone names
//! neither the crate root (`src/lib.rs` / `src/main.rs` are `crate`, not
//! `crate::lib`) nor inline `mod name { … }` blocks.
//!
//! The scan only follows modules the compiler would also see: `#[cfg]`-gated
//! modules / functions and `#[path]` modules are skipped, and only the module
//! tree that contains the `vespera!` call site is used, so stray files never
//! produce a `crate::…` path that does not resolve.  A cheap text pre-filter
//! (`cron(` anywhere under `src/`) keeps the common case — no cron jobs at
//! all — free of any parsing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::ext::IdentExt;

use crate::cron_impl::{CronArgs, StoredCronInfo, validate_cron_fn};
use crate::metadata::CronMetadata;
use crate::schema_macro::file_cache;

/// Substring every `#[cron("…")]` / `#[vespera::cron("…")]` attribute contains.
const CRON_ATTR_MARKER: &str = "cron(";

/// Build the cron job list for one `vespera!` expansion: the storage entries
/// first (unchanged order), then any job the module-tree scan finds that the
/// storage does not cover yet.
pub(super) fn discover_cron_jobs(
    storage: &[StoredCronInfo],
    src_dir: Option<&Path>,
    call_site: Option<&Path>,
) -> Vec<CronMetadata> {
    let mut canonical_paths = HashMap::new();
    // Most projects declare no `#[vespera::cron]` jobs: without stored jobs
    // or a `cron(` attribute anywhere under `src/`, nothing is parsed.
    let tree = src_dir
        .filter(|src_dir| !storage.is_empty() || has_cron_attribute(src_dir))
        .and_then(|src_dir| crate_tree(src_dir, call_site));
    let mut jobs = stored_jobs(storage, src_dir, tree.as_ref(), &mut canonical_paths);
    if let Some(tree) = tree {
        // A scanned job is known when a stored one sits in the same file with
        // the same function name AND module path — same-named jobs in
        // different inline modules of one file are distinct.
        let known: HashSet<(String, String, String)> = storage
            .iter()
            .zip(&jobs)
            .filter_map(|(s, job)| {
                s.file_path.as_deref().map(|fp| {
                    (
                        canonicalized_cron_path(fp, &mut canonical_paths),
                        s.fn_name.clone(),
                        job.module_path.clone(),
                    )
                })
            })
            .collect();
        jobs.extend(tree.jobs.into_iter().filter(|job| {
            !known.contains(&(
                job.file_path.clone(),
                job.function_name.clone(),
                job.module_path.clone(),
            ))
        }));
    }
    jobs
}

/// Convert `CRON_STORAGE` entries.  The module path is the one `tree`
/// declares the function in (in the entry's inline module when it recorded
/// one, else the file's first declaration of that name); without a match (no
/// tree, `#[path]` module, macro-generated function) it is derived from the
/// file path relative to `src/` plus the recorded inline module.
fn stored_jobs(
    storage: &[StoredCronInfo],
    src_dir: Option<&Path>,
    tree: Option<&ModuleTree>,
    canonical_paths: &mut HashMap<PathBuf, String>,
) -> Vec<CronMetadata> {
    // Skip the `fs::canonicalize` syscall on `<manifest>/src` when there is
    // nothing to place.
    if storage.is_empty() {
        return Vec::new();
    }
    let src_display = src_dir.map(canonical_display).unwrap_or_default();
    storage
        .iter()
        .map(|s| {
            let module_path = s
                .file_path
                .as_ref()
                .map(|fp| {
                    let normalized = canonicalized_cron_path(fp, canonical_paths);
                    let inline = s.inline_module.as_deref();
                    let declared =
                        tree.and_then(|tree| tree.module_of(&normalized, inline, &s.fn_name));
                    declared.unwrap_or_else(|| {
                        let relative = normalized
                            .strip_prefix(&src_display)
                            .map_or(&*normalized, |rest| rest.trim_start_matches('/'));
                        join_module(&cron_module_path(relative), inline.unwrap_or_default())
                    })
                })
                .unwrap_or_default();
            CronMetadata {
                expression: s.expression.clone(),
                function_name: s.fn_name.clone(),
                module_path,
                file_path: s.file_path.clone().unwrap_or_default(),
                options: s.options.clone(),
            }
        })
        .collect()
}

/// Whether any `.rs` file under `src/` contains a cron attribute.
fn has_cron_attribute(src_dir: &Path) -> bool {
    file_cache::list_rs_files(src_dir).iter().any(|path| {
        file_cache::get_file_content(path).is_some_and(|content| content.contains(CRON_ATTR_MARKER))
    })
}

/// The module tree that contains the `vespera!` call site; without call-site
/// information (unit tests) the library root wins over the binary root.
fn crate_tree(src_dir: &Path, call_site: Option<&Path>) -> Option<ModuleTree> {
    let call_site = call_site.map(canonical_display);
    let mut trees = ["lib.rs", "main.rs"]
        .into_iter()
        .map(|root| src_dir.join(root))
        .filter(|root| root.is_file())
        .map(|root| ModuleTree::from_root(&root, src_dir));
    match call_site {
        Some(call_site) => trees.find(|tree| tree.files.contains(&call_site)),
        None => trees.next(),
    }
}

/// Files reachable from one crate root plus the `#[cron]` jobs declared in
/// them, each with its real module path.
#[derive(Default)]
struct ModuleTree {
    files: HashSet<String>,
    /// Schedulable jobs outside `#[cfg]`-gated items — the scan result.
    jobs: Vec<CronMetadata>,
    /// (canonical file, function name) → module path of every function
    /// carrying a cron attribute, `#[cfg]`-gated ones included (rustc may
    /// have expanded those): places the stored jobs.  The first declaration
    /// wins.
    cron_fns: HashMap<(String, String), String>,
    /// (canonical file, inline-module path inside it, function name) →
    /// module path: places stored jobs that recorded their inline module,
    /// so same-named jobs in different inline modules of one file stay apart.
    scoped_cron_fns: HashMap<(String, String, String), String>,
}

/// Where [`ModuleTree::visit`] currently is.
struct Scope<'a> {
    /// Directory that child file modules resolve under.
    mod_dir: &'a Path,
    /// Module path of the items (`""` = crate root).
    module_path: &'a str,
    /// Canonical file the items live in.
    file: &'a str,
    /// Inline-module path inside `file` (`""` = file level).
    inline: &'a str,
    /// Inside a `#[cfg]`-gated module: indexed, never scanned.
    gated: bool,
}

impl ModuleTree {
    fn from_root(root: &Path, src_dir: &Path) -> Self {
        let mut tree = Self::default();
        tree.walk(root, src_dir, "", false);
        tree
    }

    /// Record `file` (module `module_path`, child modules resolved under
    /// `mod_dir`) and descend into its items.  Files without `mod` or a cron
    /// attribute cannot contribute anything, so they are never parsed.
    fn walk(&mut self, file: &Path, mod_dir: &Path, module_path: &str, gated: bool) {
        let display = canonical_display(file);
        self.files.insert(display.clone());
        if let Some(content) = file_cache::get_file_content(file)
            && (content.contains("mod") || content.contains(CRON_ATTR_MARKER))
            && let Ok(ast) = syn::parse_file(&content)
        {
            let scope = Scope {
                mod_dir,
                module_path,
                file: &display,
                inline: "",
                gated,
            };
            self.visit(&ast.items, &scope, content.contains(CRON_ATTR_MARKER));
        }
    }

    fn visit(&mut self, items: &[syn::Item], scope: &Scope<'_>, collect_jobs: bool) {
        for item in items {
            match item {
                syn::Item::Mod(module) => {
                    let name = module.ident.unraw().to_string();
                    let child_path = join_module(scope.module_path, &name);
                    let child_dir = scope.mod_dir.join(&name);
                    let gated = scope.gated || is_cfg_gated(&module.attrs);
                    if let Some((_, inner)) = &module.content {
                        let inline = join_module(scope.inline, &name);
                        let child = Scope {
                            mod_dir: &child_dir,
                            module_path: &child_path,
                            file: scope.file,
                            inline: &inline,
                            gated,
                        };
                        self.visit(inner, &child, collect_jobs);
                    } else if !module.attrs.iter().any(|a| a.path().is_ident("path"))
                        && let Some(child) = [
                            scope.mod_dir.join(format!("{name}.rs")),
                            child_dir.join("mod.rs"),
                        ]
                        .into_iter()
                        .find(|candidate| candidate.is_file())
                    {
                        self.walk(&child, &child_dir, &child_path, gated);
                    }
                }
                syn::Item::Fn(function)
                    if collect_jobs && function.attrs.iter().any(|a| is_cron_path(a.path())) =>
                {
                    self.add_cron_fn(function, scope);
                }
                _ => {}
            }
        }
    }

    /// Module path of the cron function `fn_name` declared in `file` — in
    /// the inline module `inline` when known, else its first declaration.
    fn module_of(&self, file: &str, inline: Option<&str>, fn_name: &str) -> Option<String> {
        inline
            .map_or_else(
                || self.cron_fns.get(&(file.to_owned(), fn_name.to_owned())),
                |inline| {
                    self.scoped_cron_fns.get(&(
                        file.to_owned(),
                        inline.to_owned(),
                        fn_name.to_owned(),
                    ))
                },
            )
            .cloned()
    }

    /// Index a function carrying a cron attribute and, when it is
    /// schedulable and not `#[cfg]`-gated, record it as a scanned job.
    fn add_cron_fn(&mut self, function: &syn::ItemFn, scope: &Scope<'_>) {
        let function_name = function.sig.ident.unraw().to_string();
        self.cron_fns
            .entry((scope.file.to_owned(), function_name.clone()))
            .or_insert_with(|| scope.module_path.to_owned());
        self.scoped_cron_fns
            .entry((
                scope.file.to_owned(),
                scope.inline.to_owned(),
                function_name.clone(),
            ))
            .or_insert_with(|| scope.module_path.to_owned());
        if !scope.gated
            && let Some(args) = cron_args(function)
        {
            self.jobs.push(CronMetadata {
                expression: args.expression.value(),
                function_name,
                module_path: scope.module_path.to_owned(),
                file_path: scope.file.to_owned(),
                options: args.options(),
            });
        }
    }
}

/// The arguments of a schedulable `#[cron("…", tz = "…", overlap = "…")]`
/// function, or `None` when the function is not one (the attribute macro
/// reports malformed ones itself).
fn cron_args(function: &syn::ItemFn) -> Option<CronArgs> {
    if validate_cron_fn(function).is_err() || is_cfg_gated(&function.attrs) {
        return None;
    }
    function
        .attrs
        .iter()
        .find(|attr| is_cron_path(attr.path()))
        .and_then(|attr| attr.parse_args::<CronArgs>().ok())
}

/// `cron` or `vespera::cron`.
fn is_cron_path(path: &syn::Path) -> bool {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    matches!(
        segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["cron"] | ["vespera", "cron"]
    )
}

fn is_cfg_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("cfg"))
}

fn join_module(parent: &str, name: &str) -> String {
    if name.is_empty() {
        parent.to_owned()
    } else if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}::{name}")
    }
}

/// Canonical, `/`-separated rendering used to compare scan paths with the
/// `Span::local_file()` paths `#[cron]` stored.
fn canonical_display(path: &Path) -> String {
    path.canonicalize()
        .map_or_else(
            |_| path.display().to_string(),
            |path| path.display().to_string(),
        )
        .replace('\\', "/")
}

fn canonicalized_cron_path(fp: &str, cache: &mut HashMap<PathBuf, String>) -> String {
    cache
        .entry(PathBuf::from(fp))
        .or_insert_with_key(|path| canonical_display(path))
        .clone()
}

/// Convert a `src/`-relative file path to a module path: the crate roots
/// `lib.rs` / `main.rs` are the crate itself (`""`); otherwise strip `.rs`,
/// `/` → `::`, `-` → `_`, drop a trailing `::mod`.
fn cron_module_path(relative: &str) -> String {
    if matches!(relative, "lib.rs" | "main.rs") {
        return String::new();
    }
    let stem = relative.strip_suffix(".rs").unwrap_or(relative);
    let mut module_path = String::with_capacity(stem.len());
    for ch in stem.chars() {
        match ch {
            '/' => module_path.push_str("::"),
            '-' => module_path.push('_'),
            _ => module_path.push(ch),
        }
    }
    if module_path.ends_with("::mod") {
        module_path.truncate(module_path.len() - "::mod".len());
    }
    module_path
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;
    use tempfile::TempDir;

    use crate::metadata::CronOptions;

    use super::*;

    /// Write `files` (relative to `src/`) into a fresh temp crate and return it.
    fn crate_with(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        for (relative, content) in files {
            let path = dir.path().join("src").join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    fn stored(fn_name: &str, file_path: Option<String>) -> StoredCronInfo {
        StoredCronInfo {
            fn_name: fn_name.to_owned(),
            expression: "0 0 0 * * *".to_owned(),
            file_path,
            inline_module: None,
            options: CronOptions::default(),
        }
    }

    fn stored_in(fn_name: &str, file_path: &Path, inline: &str, expr: &str) -> StoredCronInfo {
        StoredCronInfo {
            expression: expr.to_owned(),
            inline_module: Some(inline.to_owned()),
            ..stored(fn_name, Some(file_path.display().to_string()))
        }
    }

    /// Same-named jobs in different inline modules of one file: each stored
    /// entry is placed in its own module, and a sibling the storage does not
    /// know yet is still scanned.  `#[path]` files keep the recorded inline
    /// module on the derived path.
    #[test]
    fn same_named_jobs_in_inline_modules_do_not_clash() {
        let krate = crate_with(&[
            (
                "lib.rs",
                r#"
                pub mod a { #[cron("1 * * * * *")] pub async fn tick() {} }
                pub mod b { #[cron("2 * * * * *")] pub async fn tick() {} }
                pub mod c { pub mod d { #[cron("3 * * * * *")] pub async fn tick() {} } }
                #[path = "x.rs"] mod pathed;
                "#,
            ),
            (
                "x.rs",
                r#"mod e { #[cron("4 * * * * *")] pub async fn tick() {} }"#,
            ),
        ]);
        let src = krate.path().join("src");
        let lib = src.join("lib.rs");
        let storage = [
            stored_in("tick", &lib, "a", "1 * * * * *"),
            stored_in("tick", &lib, "b", "2 * * * * *"),
            stored_in("tick", &src.join("x.rs"), "e", "4 * * * * *"),
        ];
        let jobs = discover_cron_jobs(&storage, Some(&src), Some(&lib));
        assert_eq!(
            summary(&jobs),
            rows(&[
                ("a", "tick", "1 * * * * *"),
                ("b", "tick", "2 * * * * *"),
                ("c::d", "tick", "3 * * * * *"),
                ("x::e", "tick", "4 * * * * *"),
            ])
        );
    }

    fn summary(jobs: &[CronMetadata]) -> Vec<(String, String, String)> {
        let mut rows: Vec<_> = jobs
            .iter()
            .map(|job| {
                (
                    job.module_path.clone(),
                    job.function_name.clone(),
                    job.expression.clone(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    #[rstest]
    #[case::slashes_and_hyphens("admin-tools/daily-job.rs", "admin_tools::daily_job")]
    #[case::module_file("admin-tools/mod.rs", "admin_tools")]
    #[case::library_root("lib.rs", "")]
    #[case::binary_root("main.rs", "")]
    #[case::nested_lib_file("tools/lib.rs", "tools::lib")]
    fn cron_module_path_normalizes_rust_module_syntax(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(cron_module_path(input), expected);
    }

    #[test]
    fn storage_entries_without_src_dir_keep_their_derived_paths() {
        let jobs = discover_cron_jobs(
            &[
                stored("no_file", None),
                stored("missing", Some("does/not/exist.rs".to_owned())),
            ],
            None,
            None,
        );
        assert_eq!(
            summary(&jobs),
            vec![
                (
                    String::new(),
                    "no_file".to_owned(),
                    "0 0 0 * * *".to_owned()
                ),
                (
                    "does::not::exist".to_owned(),
                    "missing".to_owned(),
                    "0 0 0 * * *".to_owned()
                ),
            ]
        );
    }

    /// Jobs in modules rustc expands after `vespera!` are recovered from the
    /// module tree; storage-known, gated, `#[path]`, private, non-literal and
    /// unreachable ones are not.
    #[test]
    fn scan_recovers_unexpanded_jobs_from_the_module_tree() {
        let krate = crate_with(&[
            (
                "lib.rs",
                r#"
                mod early;
                pub fn create_app() {}
                mod late;
                mod plain;
                mod leafy;
                mod broken;
                mod missing;
                #[cfg(test)] mod gated;
                #[path = "x.rs"] mod pathed;
                pub mod inline {
                    #[vespera::cron("0 0 * * * *")]
                    pub async fn inline_job() {}
                }
                "#,
            ),
            (
                "early.rs",
                r#"#[cron("1 * * * * *")] pub async fn early_job() {}"#,
            ),
            (
                "late/mod.rs",
                r#"
                mod deeper;
                #[cron("2 * * * * *", overlap = "skip", tz = "Asia/Seoul")] pub async fn late_job() {}
                #[cron("3 * * * * *", bogus = "x")] pub async fn bad_args() {}
                #[cron("3 * * * * *")] fn private_job() {}
                #[cfg(feature = "x")] #[cron("4 * * * * *")] pub async fn gated_job() {}
                #[cron(EXPR)] pub async fn non_literal() {}
                #[other] pub async fn not_cron() {}
                #[a::cron("9 * * * * *")] pub async fn foreign_cron() {}
                "#,
            ),
            (
                "late/deeper.rs",
                r#"#[vespera::cron("5 * * * * *")] pub async fn deep_job() {}"#,
            ),
            ("plain.rs", "pub mod inner { pub async fn helper() {} }"),
            ("leafy.rs", "pub fn leaf() {}"),
            ("broken.rs", "mod {{{ cron("),
            (
                "gated.rs",
                r#"#[cron("6 * * * * *")] pub async fn gated() {}"#,
            ),
            ("x.rs", r#"#[cron("7 * * * * *")] pub async fn pathed() {}"#),
            (
                "stray.rs",
                r#"#[cron("8 * * * * *")] pub async fn stray() {}"#,
            ),
        ]);
        let src = krate.path().join("src");
        let early = src.join("early.rs").display().to_string();
        let jobs = discover_cron_jobs(&[stored("early_job", Some(early))], Some(&src), None);
        assert_eq!(
            summary(&jobs),
            vec![
                (
                    "early".to_owned(),
                    "early_job".to_owned(),
                    "0 0 0 * * *".to_owned()
                ),
                (
                    "inline".to_owned(),
                    "inline_job".to_owned(),
                    "0 0 * * * *".to_owned()
                ),
                (
                    "late".to_owned(),
                    "late_job".to_owned(),
                    "2 * * * * *".to_owned()
                ),
                (
                    "late::deeper".to_owned(),
                    "deep_job".to_owned(),
                    "5 * * * * *".to_owned()
                ),
            ]
        );
        // Scanned jobs keep their `tz` / `overlap` arguments.
        let late = jobs
            .iter()
            .find(|job| job.function_name == "late_job")
            .unwrap();
        assert_eq!(
            late.options,
            CronOptions {
                timezone: Some("Asia/Seoul".to_owned()),
                skip_overlap: true,
            }
        );
    }

    /// Without a `cron(` attribute outside the storage-covered files nothing
    /// is parsed and nothing is added.
    #[rstest]
    #[case::no_cron_anywhere(&[("lib.rs", "mod a;"), ("a.rs", "pub fn a() {}")], false)]
    #[case::all_in_storage(&[("lib.rs", "mod a;"), ("a.rs", r#"#[cron("1 * * * * *")] pub async fn a() {}"#)], true)]
    fn scan_adds_nothing_without_unexpanded_candidates(
        #[case] files: &[(&str, &str)],
        #[case] store_a: bool,
    ) {
        let krate = crate_with(files);
        let src = krate.path().join("src");
        let storage: Vec<_> = store_a
            .then(|| stored("a", Some(src.join("a.rs").display().to_string())))
            .into_iter()
            .collect();
        let jobs = discover_cron_jobs(&storage, Some(&src), None);
        assert_eq!(jobs.len(), storage.len());
    }

    /// Only the module tree holding the `vespera!` call site contributes.
    #[rstest]
    #[case::library_call_site(Some("lib.rs"), vec!["lib_job"])]
    #[case::binary_call_site(Some("main.rs"), vec!["bin_job"])]
    #[case::outside_both_trees(Some("orphan.rs"), vec![])]
    #[case::unknown_call_site_prefers_library(None, vec!["lib_job"])]
    fn scan_uses_the_tree_containing_the_call_site(
        #[case] call_site: Option<&str>,
        #[case] expected: Vec<&str>,
    ) {
        let krate = crate_with(&[
            (
                "lib.rs",
                r#"#[cron("1 * * * * *")] pub async fn lib_job() {}"#,
            ),
            (
                "main.rs",
                r#"#[cron("2 * * * * *")] pub async fn bin_job() {}"#,
            ),
            ("orphan.rs", ""),
        ]);
        let src = krate.path().join("src");
        let call_site = call_site.map(|file| src.join(file));
        let jobs = discover_cron_jobs(&[], Some(&src), call_site.as_deref());
        let names: Vec<_> = jobs.iter().map(|job| job.function_name.as_str()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn crate_without_a_root_file_contributes_nothing() {
        let krate = crate_with(&[("jobs.rs", r#"#[cron("1 * * * * *")] pub async fn job() {}"#)]);
        let src = krate.path().join("src");
        assert!(discover_cron_jobs(&[], Some(&src), None).is_empty());
    }

    fn rows(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
        let mut rows: Vec<_> = rows
            .iter()
            .map(|(module, name, expr)| {
                ((*module).to_owned(), (*name).to_owned(), (*expr).to_owned())
            })
            .collect();
        rows.sort();
        rows
    }

    /// Stored jobs are placed by the module tree: the crate root is `crate`
    /// (not `crate::lib`), inline modules keep their segments (also
    /// `#[cfg]`-gated ones rustc expanded), and a job declared after
    /// `vespera!` in a file with stored jobs is still scanned.
    #[test]
    fn stored_jobs_take_their_module_path_from_the_module_tree() {
        let krate = crate_with(&[
            (
                "lib.rs",
                r#"
                #[vespera::cron("1 * * * * *")] pub async fn root_job() {}
                pub mod jobs {
                    #[cron("2 * * * * *")] pub async fn inline_job() {}
                    pub mod deeper {
                        #[cron("3 * * * * *")] pub async fn deep_job() {}
                    }
                }
                #[path = "x.rs"] mod pathed;
                pub fn create_app() {}
                #[cron("4 * * * * *")] pub async fn after_job() {}
                #[cfg(test)]
                mod tests {
                    #[cron("5 * * * * *")] pub async fn gated_job() {}
                }
                "#,
            ),
            (
                "x.rs",
                r#"#[cron("6 * * * * *")] pub async fn pathed_job() {}"#,
            ),
        ]);
        let src = krate.path().join("src");
        let lib = src.join("lib.rs").display().to_string();
        let x = src.join("x.rs").display().to_string();
        let storage = [
            stored("root_job", Some(lib.clone())),
            stored("inline_job", Some(lib.clone())),
            stored("deep_job", Some(lib.clone())),
            stored("gated_job", Some(lib)),
            stored("pathed_job", Some(x)),
        ];
        let stored_expr = "0 0 0 * * *";
        let jobs = discover_cron_jobs(&storage, Some(&src), Some(&src.join("lib.rs")));
        assert_eq!(
            summary(&jobs),
            rows(&[
                ("", "root_job", stored_expr),
                ("jobs", "inline_job", stored_expr),
                ("jobs::deeper", "deep_job", stored_expr),
                ("tests", "gated_job", stored_expr),
                // `#[path]` modules are not followed: derived from the file.
                ("x", "pathed_job", stored_expr),
                ("", "after_job", "4 * * * * *"),
            ])
        );
    }

    /// Without a module tree to consult (call site outside both trees), a
    /// stored crate-root job still resolves to `crate`.
    #[test]
    fn stored_root_job_without_a_tree_falls_back_to_the_crate_root() {
        let krate = crate_with(&[
            (
                "main.rs",
                r#"#[cron("1 * * * * *")] pub async fn bin_job() {}"#,
            ),
            ("orphan.rs", ""),
        ]);
        let src = krate.path().join("src");
        let main = src.join("main.rs").display().to_string();
        let jobs = discover_cron_jobs(
            &[stored("bin_job", Some(main))],
            Some(&src),
            Some(&src.join("orphan.rs")),
        );
        assert_eq!(summary(&jobs), rows(&[("", "bin_job", "0 0 0 * * *")]));
    }
}
