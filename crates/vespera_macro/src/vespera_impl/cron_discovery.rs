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
//!    `#[cron("…")]` / `#[vespera::cron("…")]` (or a local alias of vespera's
//!    `cron`, `use vespera::cron as job;`, `use vespera as v;` + `#[v::cron]`)
//!    that the storage does not know yet is added.  Known jobs are matched per
//!    function, so a job declared
//!    after `vespera!` in a file whose earlier jobs are stored is found too.
//!
//! The same module tree places the stored jobs: a file path alone names
//! neither the crate root (`src/lib.rs` / `src/main.rs` are `crate`, not
//! `crate::lib`) nor inline `mod name { … }` blocks.
//!
//! The scan follows the modules the compiler would also see: `#[path = "…"]`
//! modules are followed the way rustc resolves them, and only the module tree
//! that contains the `vespera!` call site is used, so stray files never
//! produce a `crate::…` path that does not resolve. A `#[cfg]`-gated module or
//! function is followed too: its job is registered under the predicates that
//! gate it (all of them, nested gates included; see [`CronMetadata::cfg`]),
//! and one that is statically false (`#[cfg(any())]`) is never compiled, so
//! it is dropped. A cheap text pre-filter (the name `cron` or a `#[path`
//! declaration anywhere under `src/`) keeps the common case — no cron jobs at
//! all — free of any parsing.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use quote::ToTokens;
use syn::ext::IdentExt;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;

use crate::cron_impl::{CronArgs, StoredCronInfo, validate_cron_fn};
use crate::metadata::CronMetadata;
use crate::schema_macro::cfg_eval::{Predicate, compiled_out, statically_active, statically_false};
use crate::schema_macro::field_attrs::field_cfgs;
use crate::schema_macro::file_cache;

use imports::{CronImports, cron_imports};

/// Substring every `#[cron(..)]` / `#[vespera::cron(..)]` attribute contains.
/// Only a pre-filter: `#[cron ("..")]` is valid Rust too, so the marker is the
/// name alone and the parse decides what is a job.
const CRON_ATTR_MARKER: &str = "cron";

/// Substring of a glob import (`use super::*;`, `use crate::*;`). A file with
/// one may use a cron alias of its enclosing module without spelling `cron`
/// itself (see [`CronImports::inheriting`]), so it is parsed too.
const GLOB_IMPORT_MARKER: &str = "::*";

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
    // or a scan marker (a `cron` name or a `#[path]` declaration) anywhere
    // under `src/` (or, for a call site
    // outside `src/`, anywhere in the call site's directory, where its
    // module tree lives — see `outside_src_tree`), nothing is parsed.
    let tree = src_dir
        .filter(|src_dir| {
            !storage.is_empty()
                || has_scan_marker(src_dir)
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
                            "vespera! macro: cron job `{}` is declared in '{}', which is reached \
                             under more than one module path (a file more than one module \
                             includes with `#[path]`, or same-named jobs in sibling inline \
                             modules), so its module path cannot be told apart. Give the job a \
                             file of its own or a name no sibling module uses.",
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
                // Stored jobs were expanded by rustc, which already kept or
                // dropped their attributes.
                cfg: Vec::new(),
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

/// Whether any `.rs` file under `src/` contains a cron attribute or declares a
/// `#[path]` module. A `#[path]` module may load a job from outside `src/`
/// (`#[path = "../shared/jobs.rs"] mod jobs;`), which only the tree walk reads.
fn has_scan_marker(src_dir: &Path) -> bool {
    file_cache::list_rs_files(src_dir).iter().any(|path| {
        file_cache::get_file_content(path).is_some_and(|content| {
            content.contains(CRON_ATTR_MARKER) || declares_path_attribute(&content)
        })
    })
}

/// Whether some `#[` attribute in `content` is `path` (`#[path = ...]`, or
/// spaced like `#[ path = ...]`). A false positive (`#[pathological]`) only
/// costs a tree walk.
fn declares_path_attribute(content: &str) -> bool {
    content
        .match_indices("#[")
        .any(|(at, _)| content[at + 2..].trim_start().starts_with("path"))
}

/// Whether `call_site` lies outside `src_dir` (`examples/demo/main.rs`) and
/// its directory has a scan marker (the name `cron` or a `#[path]` module): the call-site file itself or
/// a module it declares (`examples/demo/jobs.rs`), which the `src/` check
/// never reads.
fn outside_src_has_cron(src_dir: &Path, call_site: &Path) -> bool {
    let src_prefix = format!("{}/", canonical_display(src_dir));
    !canonical_display(call_site).starts_with(&src_prefix)
        && call_site.parent().is_some_and(has_scan_marker)
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
    /// Schedulable jobs (a statically dead `#[cfg]` one is dropped) — the scan
    /// result.
    jobs: Vec<CronMetadata>,
    /// (canonical file, function name) → module path of every file-level
    /// function carrying a cron attribute, `#[cfg]`-gated ones included
    /// (rustc may have expanded those): places the stored jobs that record no
    /// inline module.  The first declaration wins.  A statically dead
    /// declaration is never indexed (see `index`).
    cron_fns: HashMap<(String, String), String>,
    /// The same for functions in an inline module, keyed like `cron_fns`: a
    /// stored job that records no inline module falls back to it when the
    /// file itself declares no such function.
    inline_cron_fns: HashMap<(String, String), String>,
    /// (canonical file, inline-module path inside it, function name) →
    /// module path: places stored jobs that recorded their inline module,
    /// so same-named jobs in different inline modules of one file stay apart.
    scoped_cron_fns: HashMap<(String, String, String), String>,
    /// (canonical file, function name) of a cron function reached under two
    /// different module paths: a file that two `mod` declarations include
    /// with `#[path]`, or a same-named job in two sibling inline modules.
    /// A stored job of it that does not record its inline module cannot name
    /// one module.
    ambiguous_cron_fns: HashSet<(String, String)>,
    /// The same for `inline_cron_fns`.
    ambiguous_inline_cron_fns: HashSet<(String, String)>,
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
    /// The `#[cfg]` predicates of the enclosing modules (all must hold).
    gate: CfgGate,
    /// The enclosing module's effective `use` state: what its `use super::*;`
    /// glob brings in.
    parent: Inherited<'a>,
    /// The crate root's effective `use` state: what its `use crate::*;` glob
    /// brings in (the root itself sees the empty default).
    root: Inherited<'a>,
}

/// A module's effective `use` state, as a glob of a child module inherits it:
/// whether its bare `cron` is another crate's (see [`CronImports::foreign`]),
/// and the imports in effect (see [`CronImports::inheriting`]).
#[derive(Clone, Copy)]
struct Inherited<'a> {
    foreign: bool,
    imports: &'a CronImports,
}

impl ModuleTree {
    fn from_root(root: &Path, src_dir: &Path) -> Self {
        let mut tree = Self::default();
        let empty = CronImports::default();
        let none = Inherited {
            foreign: false,
            imports: &empty,
        };
        tree.walk(root, src_dir, "", &CfgGate::default(), none, none);
        tree
    }

    /// Record `file` (module `module_path`, child modules resolved under
    /// `mod_dir`) and descend into its items.  Files without `mod` or a cron
    /// attribute cannot contribute anything, so they are never parsed.
    fn walk(
        &mut self,
        file: &Path,
        mod_dir: &Path,
        module_path: &str,
        gate: &CfgGate,
        parent: Inherited<'_>,
        root: Inherited<'_>,
    ) {
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
                gate: gate.inside(&summary.module.gate),
                parent,
                root,
            };
            self.visit(&summary.module, &scope);
        }
        self.stack.pop();
    }

    fn visit(&mut self, module: &ModuleSummary, scope: &Scope<'_>) {
        let foreign = module
            .imports
            .foreign(scope.parent.foreign, scope.root.foreign);
        let imports = module
            .imports
            .inheriting(scope.parent.imports, scope.root.imports);
        let own = Inherited {
            foreign,
            imports: &imports,
        };
        // The crate root is the module with an empty path: its own state is
        // what every `use crate::*;` below it inherits.
        let root = if scope.module_path.is_empty() {
            own
        } else {
            scope.root
        };
        for item in &module.items {
            match item {
                ItemSummary::Mod {
                    name,
                    gate,
                    path,
                    inline,
                } => {
                    let child_path = join_module(scope.module_path, name);
                    let gate = gate.inside(&scope.gate);
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
                            gate,
                            parent: own,
                            root,
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
                        // `..` is folded lexically: a verbatim-prefixed base is not
                        // normalised by the filesystem, so `is_file` would miss it.
                        let child = fold_parent_dirs(&base.join(path));
                        if child.is_file() {
                            let dir = child.parent().unwrap_or(base).to_path_buf();
                            self.walk(&child, &dir, &child_path, &gate, own, root);
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
                            self.walk(&child, &child_dir, &child_path, &gate, own, root);
                        }
                    }
                }
                ItemSummary::Cron { name, gate, attrs } => {
                    // The attributes this module's imports (its own, and the
                    // aliases its globs inherit) name vespera's `cron`. The job
                    // is the first of them, skipping a foreign bare `#[cron]`;
                    // a fn none of them names is not a cron fn at all.
                    let claimed: Vec<&CronAttr> = attrs
                        .iter()
                        .filter(|attr| imports.is_cron_segments(&attr.segments))
                        .collect();
                    if !claimed.is_empty() {
                        let job = claimed
                            .iter()
                            .find(|attr| !(foreign && attr.is_bare()))
                            .and_then(|attr| attr.job.as_ref());
                        self.add_cron_fn(name, job, &gate.inside(&scope.gate), scope);
                    }
                }
            }
        }
    }
}

/// The `#[cfg]` gate of an item the scan reads, as [`CronMetadata::cfg`]
/// needs it: `dead` when a predicate is statically false (rustc never compiles
/// the item), else the predicates still undecided (a statically true one holds
/// and is dropped). Nested gates combine with [`CfgGate::inside`].
#[derive(Clone, Default)]
struct CfgGate {
    dead: bool,
    /// Token text of each undecided predicate, outermost first.
    predicates: Vec<String>,
}

impl CfgGate {
    fn of(attrs: &[syn::Attribute]) -> Self {
        let predicates = field_cfgs(attrs)
            .iter()
            .filter_map(|attr| attr.parse_args::<Predicate>().ok())
            .filter(|predicate| !statically_active(predicate))
            .map(|predicate| predicate.to_token_stream().to_string())
            .collect();
        Self {
            dead: compiled_out(attrs),
            predicates,
        }
    }

    /// This item's gate inside `outer`, the gate of the enclosing module: both
    /// must hold, so the job is dead if either is.
    fn inside(&self, outer: &Self) -> Self {
        Self {
            dead: self.dead || outer.dead,
            predicates: outer
                .predicates
                .iter()
                .chain(&self.predicates)
                .cloned()
                .collect(),
        }
    }
}

/// A schedulable job's expression, options and registration `cfg` predicates
/// (see [`cron_candidates`]).
type CronJob = (String, crate::metadata::CronOptions, Vec<String>);

/// What the module-tree walk reads off one file: its items (cron functions
/// only when the file can hold jobs: the name `cron` occurs in it).  Plain data
/// derived from one parse, kept while the file is unchanged
/// (`file_cache::derived`): every `vespera!` walks the whole crate,
/// route-cache hits included.
struct FileSummary {
    module: ModuleSummary,
}

/// The items of one module (a file or an inline `mod`) and its `use` decisions.
struct ModuleSummary {
    imports: CronImports,
    items: Vec<ItemSummary>,
    /// The file's own inner `#![cfg(..)]` gate (empty for an inline `mod`,
    /// whose gate is on its declaration).
    gate: CfgGate,
}

/// One item the walk reads: a `mod` or a function carrying a cron attribute.
enum ItemSummary {
    Mod {
        /// Unraw name.
        name: String,
        /// Its `#[cfg]` gate.
        gate: CfgGate,
        /// Its `#[path = "…"]`.
        path: Option<String>,
        /// The items of an inline `mod m { .. }`.
        inline: Option<ModuleSummary>,
    },
    Cron {
        /// Unraw name.
        name: String,
        /// Its `#[cfg]` gate.
        gate: CfgGate,
        /// The attributes of the fn that may name vespera's `cron`, in source
        /// order (see [`cron_attrs`]).
        attrs: Vec<CronAttr>,
    },
}

/// One attribute of a fn that may be a `#[cron]`, whichever path it is spelled
/// with: which of them is one depends on the imports of the module the fn is
/// in, so that is decided at walk time.
struct CronAttr {
    /// The path the attribute is spelled with (`cron`, `job`, `v::cron`).
    segments: Vec<String>,
    /// Its job when its arguments parse and the fn is schedulable (see
    /// [`cron_attrs`]).
    job: Option<CronJob>,
}

impl CronAttr {
    /// Written bare (`#[cron]`, not `#[vespera::cron]`).
    fn is_bare(&self) -> bool {
        matches!(self.segments.as_slice(), [name] if name == "cron")
    }
}

/// The cached [`FileSummary`]: `None` for a file without `mod` or a cron
/// attribute (nothing to contribute, never parsed) or one that does not
/// parse.
struct CachedSummary(Option<std::rc::Rc<FileSummary>>);

fn file_summary(file: &Path) -> Option<std::rc::Rc<FileSummary>> {
    file_cache::derived(file, |content| {
        let collect_jobs =
            content.contains(CRON_ATTR_MARKER) || content.contains(GLOB_IMPORT_MARKER);
        CachedSummary(
            (content.contains("mod") || collect_jobs)
                .then(|| syn::parse_file(content).ok())
                .flatten()
                .map(|ast| {
                    let mut module = summarize(&ast.items, collect_jobs);
                    // The file's own inner `#![cfg(..)]` gates every job in it,
                    // as its declaration's `#[cfg(..)]` does.
                    module.gate = CfgGate::of(&ast.attrs);
                    std::rc::Rc::new(FileSummary { module })
                }),
        )
    })?
    .0
    .clone()
}

fn summarize(items: &[syn::Item], collect_jobs: bool) -> ModuleSummary {
    let imports = cron_imports(items);
    let items = items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) => Some(ItemSummary::Mod {
                name: module.ident.unraw().to_string(),
                gate: CfgGate::of(&module.attrs),
                path: path_attr(&module.attrs),
                inline: module
                    .content
                    .as_ref()
                    .map(|(_, inner)| summarize(inner, collect_jobs)),
            }),
            syn::Item::Fn(function)
                if collect_jobs
                    && function
                        .attrs
                        .iter()
                        .any(|attr| !cron_candidates(attr).is_empty()) =>
            {
                Some(ItemSummary::Cron {
                    name: function.sig.ident.unraw().to_string(),
                    gate: CfgGate::of(&function.attrs),
                    attrs: cron_attrs(function),
                })
            }
            _ => None,
        })
        .collect();
    ModuleSummary {
        imports,
        items,
        gate: CfgGate::default(),
    }
}

/// The [`CronAttr`]s of a fn that may be a `#[cron]` (see [`cron_candidates`]).
/// A fn that is not schedulable (the attribute macro reports its malformed
/// attributes itself) keeps its candidates, so the walk still indexes it as it
/// always did, but none of them has a job.
fn cron_attrs(function: &syn::ItemFn) -> Vec<CronAttr> {
    let schedulable = validate_cron_fn(function).is_ok();
    function
        .attrs
        .iter()
        .flat_map(cron_candidates)
        .map(|mut attr| {
            if !schedulable {
                attr.job = None;
            }
            attr
        })
        .collect()
}

/// The [`CronAttr`]s one attribute of a fn may carry: itself, or the metas of a
/// `cfg_attr(p, ..)`. A path with at most two segments (`cron`, `job`,
/// `v::cron`) may name vespera's `cron` through an import, which only the
/// walk knows ([`CronImports::is_cron_segments`]). The compiler keeps the
/// `cfg_attr` metas unless `p` is statically false, so the scan drops them
/// then. A `p` that is statically true makes a meta a plain job; an undecided
/// `p` (`windows`, `test`, `target_*`, ..) keeps the job too, registered under
/// `#[cfg(p)]` (see [`CronMetadata::cfg`]) so rustc decides exactly as it does
/// for the attribute.
fn cron_candidates(attr: &syn::Attribute) -> Vec<CronAttr> {
    if !attr.path().is_ident("cfg_attr") {
        return if may_name_cron(attr.path()) {
            vec![cron_attr(attr.path(), attr.parse_args::<CronArgs>(), &[])]
        } else {
            Vec::new()
        };
    }
    let Ok((predicate, metas)) = attr.parse_args_with(cfg_attr_parts) else {
        return Vec::new();
    };
    if statically_false(&predicate) {
        return Vec::new();
    }
    let gate = if statically_active(&predicate) {
        Vec::new()
    } else {
        vec![predicate.to_token_stream().to_string()]
    };
    metas
        .iter()
        .filter(|meta| may_name_cron(meta.path()))
        .map(|meta| {
            let args = meta
                .require_list()
                .and_then(syn::MetaList::parse_args::<CronArgs>);
            cron_attr(meta.path(), args, &gate)
        })
        .collect()
}

/// Whether `path` could be vespera's `cron` spelled through an import: at most
/// two segments (`cron`, a local alias, `v::cron`).
fn may_name_cron(path: &syn::Path) -> bool {
    path.segments.len() <= 2
}

/// `cfg_attr(predicate, a, b, ..)`: the predicate and the attributes it adds.
fn cfg_attr_parts(
    input: ParseStream<'_>,
) -> syn::Result<(Predicate, Punctuated<syn::Meta, syn::Token![,]>)> {
    let predicate: Predicate = input.parse()?;
    input.parse::<syn::Token![,]>()?;
    Ok((predicate, Punctuated::parse_terminated(input)?))
}

fn cron_attr(path: &syn::Path, args: syn::Result<CronArgs>, gate: &[String]) -> CronAttr {
    CronAttr {
        segments: path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect(),
        job: args
            .ok()
            .map(|args| (args.expression.value(), args.options(), gate.to_vec())),
    }
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

/// `path` with its `.` segments dropped and each `..` popping the segment
/// before it (lexical: the file need not exist). Absolute bases only: a `..`
/// with nothing left to pop is dropped, so it never escapes the base.
fn fold_parent_dirs(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    folded
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

mod imports;
mod index;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_alias;
#[cfg(test)]
mod tests_gate;
#[cfg(test)]
mod tests_index;
