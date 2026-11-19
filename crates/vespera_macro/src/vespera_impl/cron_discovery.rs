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
//! modules / functions are skipped, `#[path = "…"]` modules are followed the
//! way rustc resolves them, and only the module
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

/// Substring every `#[cron(..)]` / `#[vespera::cron(..)]` attribute contains.
/// Only a pre-filter: `#[cron ("..")]` is valid Rust too, so the marker is the
/// name alone and the parse decides what is a job.
const CRON_ATTR_MARKER: &str = "cron";

/// Build the cron job list for one `vespera!` expansion: the storage entries
/// first (unchanged order), then any job the module-tree scan finds that the
/// storage does not cover yet.
pub(super) fn discover_cron_jobs(
    storage: &[StoredCronInfo],
    src_dir: Option<&Path>,
    call_site: Option<&Path>,
) -> Result<Vec<CronMetadata>, String> {
    let mut canonical_paths = HashMap::new();
    // Most projects declare no `#[vespera::cron]` jobs: without stored jobs
    // or a `cron` attribute anywhere under `src/` (or, for a call site
    // outside `src/`, anywhere in the call site's directory, where its
    // module tree lives — see `outside_src_tree`), nothing is parsed.
    let tree = src_dir
        .filter(|src_dir| {
            !storage.is_empty()
                || has_cron_attribute(src_dir)
                || call_site.is_some_and(|call_site| outside_src_has_cron(src_dir, call_site))
        })
        .and_then(|src_dir| crate_tree(src_dir, call_site));
    let storage = same_target(
        storage,
        tree.as_ref(),
        crate::macro_storage::in_long_lived_server(),
        &mut canonical_paths,
    );
    let storage: &[StoredCronInfo] = &storage;
    let placement = Placement {
        src_dir,
        tree: tree.as_ref(),
        call_site: call_site.map(canonical_display),
    };
    let mut jobs = stored_jobs(storage, &placement, &mut canonical_paths)?;
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
    Ok(jobs)
}

/// The stored jobs of the target being expanded.
///
/// The storage is keyed by manifest directory, so inside rust-analyzer's
/// long-lived proc-macro server the lib, binaries, examples and tests of one
/// package share it: there (`filter`), an entry whose file is outside the
/// call site's module tree belongs to another target and is dropped.  Under
/// rustc the storage only holds the current compilation and nothing is
/// filtered (a file the walk cannot reach — a macro-declared module — must
/// keep its job).
fn same_target<'a>(
    storage: &'a [StoredCronInfo],
    tree: Option<&ModuleTree>,
    filter: bool,
    canonical_paths: &mut HashMap<PathBuf, String>,
) -> std::borrow::Cow<'a, [StoredCronInfo]> {
    let Some(tree) = tree.filter(|_| filter) else {
        return std::borrow::Cow::Borrowed(storage);
    };
    std::borrow::Cow::Owned(
        storage
            .iter()
            .filter(|stored| {
                stored.file_path.as_deref().is_none_or(|file| {
                    tree.files
                        .contains(&canonicalized_cron_path(file, canonical_paths))
                })
            })
            .cloned()
            .collect(),
    )
}

/// Where the crate's sources are, for placing stored jobs.
struct Placement<'a> {
    src_dir: Option<&'a Path>,
    tree: Option<&'a ModuleTree>,
    /// Canonical display of the `vespera!` call-site file.
    call_site: Option<String>,
}

/// Convert `CRON_STORAGE` entries.  The module path is the one `tree`
/// declares the function in (in the entry's inline module when it recorded
/// one, else the file's first declaration of that name); without a match (no
/// tree, macro-generated function) it is derived from the
/// file path relative to `src/` plus the recorded inline module.  A file
/// outside `src/` (an example, an integration test, a custom `[lib] path`) is
/// placed relative to the call-site file, which is then the crate root; one
/// that is neither is a compile error — an absolute path is no module path.
fn stored_jobs(
    storage: &[StoredCronInfo],
    placement: &Placement<'_>,
    canonical_paths: &mut HashMap<PathBuf, String>,
) -> Result<Vec<CronMetadata>, String> {
    // Skip the `fs::canonicalize` syscall on `<manifest>/src` when there is
    // nothing to place.
    if storage.is_empty() {
        return Ok(Vec::new());
    }
    let src_display = placement.src_dir.map(canonical_display).unwrap_or_default();
    storage
        .iter()
        .map(|s| {
            let module_path = match s.file_path.as_ref() {
                Some(fp) => {
                    let normalized = canonicalized_cron_path(fp, canonical_paths);
                    let inline = s.inline_module.as_deref();
                    if placement
                        .tree
                        .is_some_and(|tree| tree.is_ambiguous(&normalized, inline, &s.fn_name))
                    {
                        return Err(format!(
                            "vespera! macro: cron job `{}` is declared in '{}', a file that more \
                             than one module includes (`#[path]`), so its module path cannot be \
                             told apart. Give the job a file of its own.",
                            s.fn_name, normalized
                        ));
                    }
                    let declared = placement
                        .tree
                        .and_then(|tree| tree.module_of(&normalized, inline, &s.fn_name));
                    match declared {
                        Some(declared) => declared,
                        None => join_module(
                            &derived_module_path(&normalized, &src_display, placement, s)?,
                            inline.unwrap_or_default(),
                        ),
                    }
                }
                None => String::new(),
            };
            Ok(CronMetadata {
                expression: s.expression.clone(),
                function_name: s.fn_name.clone(),
                module_path,
                file_path: s.file_path.clone().unwrap_or_default(),
                options: s.options.clone(),
            })
        })
        .collect()
}

/// Module path of a stored job's file the module tree did not place:
/// relative to `src/`, else relative to the call-site file's directory with
/// the call-site file itself as the crate root.
fn derived_module_path(
    normalized: &str,
    src_display: &str,
    placement: &Placement<'_>,
    stored: &StoredCronInfo,
) -> Result<String, String> {
    // Without a known `src/` (no `CARGO_MANIFEST_DIR`) only a relative path is
    // its own module path: an absolute one is no path under `src/` at all.
    let under_src = if src_display.is_empty() {
        (!Path::new(normalized).is_absolute()).then_some(normalized)
    } else {
        normalized
            .strip_prefix(src_display)
            .filter(|rest| rest.starts_with('/'))
    };
    if let Some(rest) = under_src {
        return Ok(cron_module_path(rest.trim_start_matches('/')));
    }
    let call_site = placement.call_site.as_deref().unwrap_or_default();
    if normalized == call_site {
        return Ok(String::new());
    }
    let call_site_dir = call_site.rsplit_once('/').map_or("", |(dir, _)| dir);
    match normalized.strip_prefix(call_site_dir) {
        Some(rest) if !call_site_dir.is_empty() && rest.starts_with('/') => {
            Ok(cron_module_path(rest.trim_start_matches('/')))
        }
        _ => Err(format!(
            "vespera! macro: cannot place `#[cron]` job `{}` from '{normalized}': the file is \
             neither under '{src_display}' nor next to the `vespera!` call site '{call_site}', \
             so it has no module path in this crate — declare the job in a module of the crate \
             that calls `vespera!`",
            stored.fn_name
        )),
    }
}

/// Whether any `.rs` file under `src/` contains a cron attribute.
fn has_cron_attribute(src_dir: &Path) -> bool {
    file_cache::list_rs_files(src_dir).iter().any(|path| {
        file_cache::get_file_content(path).is_some_and(|content| content.contains(CRON_ATTR_MARKER))
    })
}

/// Whether `call_site` lies outside `src_dir` (`examples/demo/main.rs`) and
/// a `cron(` attribute occurs in its directory: the call-site file itself or
/// a module it declares (`examples/demo/jobs.rs`), which the `src/` check
/// never reads.
fn outside_src_has_cron(src_dir: &Path, call_site: &Path) -> bool {
    let src_prefix = format!("{}/", canonical_display(src_dir));
    !canonical_display(call_site).starts_with(&src_prefix)
        && call_site.parent().is_some_and(has_cron_attribute)
}

/// The module tree that contains the `vespera!` call site; without call-site
/// information (unit tests) the library root wins over the binary root.
///
/// The crate root rustc is compiling (its command line) is tried first: a
/// module shared by the library and a binary (`mod app;` in both
/// `src/lib.rs` and `src/main.rs`) is in both trees, and only the tree of
/// the crate being built has its `mod`s — the binary build must not
/// schedule `crate::jobs::tick` that only the library declares.
fn crate_tree(src_dir: &Path, call_site: Option<&Path>) -> Option<ModuleTree> {
    let call_site_display = call_site.map(canonical_display);
    let mut trees = crate_roots(src_dir, call_site_display.is_some())
        .into_iter()
        .filter(|root| root.is_file())
        .map(|root| {
            // A crate root's `mod` declarations resolve next to it
            // (`src/bin/server.rs` → `src/bin/<name>.rs`).
            let mod_dir = root.parent().unwrap_or(src_dir).to_path_buf();
            ModuleTree::from_root(&root, &mod_dir)
        });
    match (call_site, call_site_display) {
        (Some(call_site), Some(display)) => {
            compiler_root_tree(&crate::env_access::compiler_args(), &display)
                .or_else(|| trees.find(|tree| tree.files.contains(&display)))
                .or_else(|| outside_src_tree(src_dir, call_site, &display))
        }
        _ => trees.next(),
    }
}

/// The module tree of the crate root rustc compiles (its positional `.rs`
/// input) when that tree contains the call site: a custom `[lib] path` /
/// `[[bin]] path` under `src/` (`src/api.rs`) is no `lib.rs` / `main.rs`.
fn compiler_root_tree(args: &[String], call_site: &str) -> Option<ModuleTree> {
    let root = args
        .iter()
        .map(Path::new)
        .find(|arg| arg.extension().is_some_and(|ext| ext == "rs") && arg.is_file())?;
    let tree = ModuleTree::from_root(root, root.parent()?);
    tree.files.contains(call_site).then_some(tree)
}

/// A `vespera!` call site outside `src/` — an example, an integration test,
/// a bench or a custom `[lib] path` — is (by far most often) its crate's root
/// file: its module tree starts there, with child modules next to it.
fn outside_src_tree(src_dir: &Path, call_site: &Path, display: &str) -> Option<ModuleTree> {
    let src_prefix = format!("{}/", canonical_display(src_dir));
    if display.starts_with(&src_prefix) {
        return None;
    }
    let mod_dir = call_site.parent()?;
    Some(ModuleTree::from_root(call_site, mod_dir))
}

/// The crate roots Cargo discovers under `src/`: `lib.rs`, `main.rs` and —
/// only when a call site can pick among them — every binary in `src/bin/`
/// (`<name>.rs` and `<name>/main.rs`), in name order.
fn crate_roots(src_dir: &Path, with_binaries: bool) -> Vec<PathBuf> {
    let mut roots = vec![src_dir.join("lib.rs"), src_dir.join("main.rs")];
    if with_binaries && let Ok(entries) = std::fs::read_dir(src_dir.join("bin")) {
        let mut binaries: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .map(|path| {
                if path.is_dir() {
                    path.join("main.rs")
                } else {
                    path
                }
            })
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .collect();
        binaries.sort();
        roots.extend(binaries);
    }
    roots
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
    /// (canonical file, function name) of a cron function reached under two
    /// different module paths (a file that two `mod` declarations include
    /// with `#[path]`): a stored job of it cannot name one module.
    ambiguous_cron_fns: HashSet<(String, String)>,
    /// The same, keyed like `scoped_cron_fns`.
    ambiguous_scoped_cron_fns: HashSet<(String, String, String)>,
    /// The files on the current walk, outermost first: a module cycle is a
    /// file already on it (`files` keeps every file seen, for lookups).
    stack: Vec<String>,
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
        // A module cycle (`#[path = "lib.rs"] mod again;`, a symlinked
        // directory) is rustc's error; the walk must not recurse forever. A
        // file that two sibling declarations include is walked once per
        // declaration: each one is a module of its own.
        if self.stack.contains(&display) {
            return;
        }
        self.files.insert(display.clone());
        self.stack.push(display.clone());
        if let Some(summary) = file_summary(file) {
            let scope = Scope {
                mod_dir,
                module_path,
                file: &display,
                inline: "",
                gated,
            };
            self.visit(&summary.items, &scope);
        }
        self.stack.pop();
    }

    fn visit(&mut self, items: &[ItemSummary], scope: &Scope<'_>) {
        for item in items {
            match item {
                ItemSummary::Mod {
                    name,
                    cfg_gated,
                    path,
                    inline,
                } => {
                    let child_path = join_module(scope.module_path, name);
                    let gated = scope.gated || *cfg_gated;
                    if let Some(inner) = inline {
                        // rustc: `#[path = "dir"] mod m { .. }` resolves the
                        // block's file modules under `dir`, not `m`.
                        let child_dir = scope.mod_dir.join(path.as_deref().unwrap_or(name));
                        let inline = join_module(scope.inline, name);
                        let child = Scope {
                            mod_dir: &child_dir,
                            module_path: &child_path,
                            file: scope.file,
                            inline: &inline,
                            gated,
                        };
                        self.visit(inner, &child);
                    } else if let Some(path) = path {
                        // rustc: relative to the declaring file's directory at
                        // file level, to the inline modules' directory inside
                        // them; the loaded file's children resolve next to it.
                        let base = if scope.inline.is_empty() {
                            Path::new(scope.file).parent().unwrap_or(scope.mod_dir)
                        } else {
                            scope.mod_dir
                        };
                        let child = base.join(path);
                        if child.is_file() {
                            let dir = child.parent().unwrap_or(base).to_path_buf();
                            self.walk(&child, &dir, &child_path, gated);
                        }
                    } else {
                        let child_dir = scope.mod_dir.join(name);
                        if let Some(child) = [
                            scope.mod_dir.join(format!("{name}.rs")),
                            child_dir.join("mod.rs"),
                        ]
                        .into_iter()
                        .find(|candidate| candidate.is_file())
                        {
                            self.walk(&child, &child_dir, &child_path, gated);
                        }
                    }
                }
                ItemSummary::Cron { name, job } => {
                    self.add_cron_fn(name, job.as_ref(), scope);
                }
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

    /// Whether the cron function `fn_name` of `file` (in the inline module
    /// `inline` when known) is reached under two module paths.
    fn is_ambiguous(&self, file: &str, inline: Option<&str>, fn_name: &str) -> bool {
        inline.map_or_else(
            || {
                self.ambiguous_cron_fns
                    .contains(&(file.to_owned(), fn_name.to_owned()))
            },
            |inline| {
                self.ambiguous_scoped_cron_fns.contains(&(
                    file.to_owned(),
                    inline.to_owned(),
                    fn_name.to_owned(),
                ))
            },
        )
    }

    /// Index a function carrying a cron attribute and, when it is
    /// schedulable and not `#[cfg]`-gated, record it as a scanned job.
    fn add_cron_fn(&mut self, function_name: &str, job: Option<&CronJob>, scope: &Scope<'_>) {
        let key = (scope.file.to_owned(), function_name.to_owned());
        match self.cron_fns.get(&key) {
            Some(first) if first != scope.module_path => {
                self.ambiguous_cron_fns.insert(key);
            }
            Some(_) => {}
            None => {
                self.cron_fns.insert(key, scope.module_path.to_owned());
            }
        }
        let scoped_key = (
            scope.file.to_owned(),
            scope.inline.to_owned(),
            function_name.to_owned(),
        );
        match self.scoped_cron_fns.get(&scoped_key) {
            Some(first) if first != scope.module_path => {
                self.ambiguous_scoped_cron_fns.insert(scoped_key);
            }
            Some(_) => {}
            None => {
                self.scoped_cron_fns
                    .insert(scoped_key, scope.module_path.to_owned());
            }
        }
        if !scope.gated
            && let Some((expression, options)) = job
        {
            self.jobs.push(CronMetadata {
                expression: expression.clone(),
                function_name: function_name.to_owned(),
                module_path: scope.module_path.to_owned(),
                file_path: scope.file.to_owned(),
                options: options.clone(),
            });
        }
    }
}

/// A schedulable job's expression and options (see [`cron_args`]).
type CronJob = (String, crate::metadata::CronOptions);

/// What the module-tree walk reads off one file: its items (cron functions
/// only when the file can hold jobs: `cron(` occurs in it).  Plain data
/// derived from one parse, kept while the file is unchanged
/// (`file_cache::derived`): every `vespera!` walks the whole crate,
/// route-cache hits included.
struct FileSummary {
    items: Vec<ItemSummary>,
}

/// One item the walk reads: a `mod` or a function carrying a cron attribute.
enum ItemSummary {
    Mod {
        /// Unraw name.
        name: String,
        /// Carries a `#[cfg]`.
        cfg_gated: bool,
        /// Its `#[path = "…"]`.
        path: Option<String>,
        /// The items of an inline `mod m { .. }`.
        inline: Option<Vec<Self>>,
    },
    Cron {
        /// Unraw name.
        name: String,
        /// Its job when schedulable (see [`cron_args`]).
        job: Option<CronJob>,
    },
}

/// The cached [`FileSummary`]: `None` for a file without `mod` or a cron
/// attribute (nothing to contribute, never parsed) or one that does not
/// parse.
struct CachedSummary(Option<std::rc::Rc<FileSummary>>);

fn file_summary(file: &Path) -> Option<std::rc::Rc<FileSummary>> {
    file_cache::derived(file, |content| {
        let collect_jobs = content.contains(CRON_ATTR_MARKER);
        CachedSummary(
            (content.contains("mod") || collect_jobs)
                .then(|| syn::parse_file(content).ok())
                .flatten()
                .map(|ast| {
                    std::rc::Rc::new(FileSummary {
                        items: summarize(&ast.items, collect_jobs),
                    })
                }),
        )
    })?
    .0
    .clone()
}

fn summarize(items: &[syn::Item], collect_jobs: bool) -> Vec<ItemSummary> {
    let foreign_cron = imports_foreign_cron(items);
    items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) => Some(ItemSummary::Mod {
                name: module.ident.unraw().to_string(),
                cfg_gated: is_cfg_gated(&module.attrs),
                path: path_attr(&module.attrs),
                inline: module
                    .content
                    .as_ref()
                    .map(|(_, inner)| summarize(inner, collect_jobs)),
            }),
            syn::Item::Fn(function)
                if collect_jobs && function.attrs.iter().any(|a| is_cron_path(a.path())) =>
            {
                Some(ItemSummary::Cron {
                    name: function.sig.ident.unraw().to_string(),
                    job: cron_args(function, foreign_cron)
                        .map(|args| (args.expression.value(), args.options())),
                })
            }
            _ => None,
        })
        .collect()
}

/// The arguments of a schedulable `#[cron("…", tz = "…", overlap = "…")]`
/// function, or `None` when the function is not one (the attribute macro
/// reports malformed ones itself).  With `foreign_cron` (the module imports
/// another crate's `cron`) a bare `#[cron]` is not vespera's.
fn cron_args(function: &syn::ItemFn, foreign_cron: bool) -> Option<CronArgs> {
    if validate_cron_fn(function).is_err() || is_cfg_gated(&function.attrs) {
        return None;
    }
    function
        .attrs
        .iter()
        .find(|attr| {
            is_cron_path(attr.path()) && !(foreign_cron && attr.path().segments.len() == 1)
        })
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

/// Whether `items` import a `cron` from another crate (`use other::cron;`,
/// `use other::{cron};`, `use other::job as cron;`) — then a bare `#[cron]`
/// there is that crate's attribute.  Imports rooted at `vespera`, `crate`,
/// `self` or `super` and globs keep it vespera's.
fn imports_foreign_cron(items: &[syn::Item]) -> bool {
    fn imports(tree: &syn::UseTree, root: Option<&syn::Ident>) -> bool {
        let foreign = || {
            root.is_some_and(|root| {
                !matches!(
                    root.to_string().as_str(),
                    "vespera" | "crate" | "self" | "super"
                )
            })
        };
        match tree {
            syn::UseTree::Path(path) => imports(&path.tree, root.or(Some(&path.ident))),
            syn::UseTree::Name(name) => name.ident == "cron" && foreign(),
            syn::UseTree::Rename(rename) => rename.rename == "cron" && foreign(),
            syn::UseTree::Group(group) => group.items.iter().any(|tree| imports(tree, root)),
            // A glob may bring in another crate's `cron`; only vespera's (or
            // this crate's own) globs leave the bare form vespera's.
            syn::UseTree::Glob(_) => foreign(),
        }
    }
    items
        .iter()
        .any(|item| matches!(item, syn::Item::Use(item) if imports(&item.tree, None)))
}

/// The value of a `#[path = "…"]` attribute.
fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| match &attr.meta {
        syn::Meta::NameValue(syn::MetaNameValue {
            path,
            value:
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(value),
                    ..
                }),
            ..
        }) if path.is_ident("path") => Some(value.value()),
        _ => None,
    })
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
    file_cache::canonicalize_cached(path)
        .map_or_else(
            || path.display().to_string(),
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
mod tests;
