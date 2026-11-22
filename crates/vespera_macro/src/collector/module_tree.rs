//! The `mod` chain of each route file in the route folder, read off the
//! declarations that load it.
//!
//! [`ModuleTable::new`] walks the declarations from the folder's own declarers
//! (`<folder>/mod.rs`, and `<folder>.rs` beside the folder) with rustc's
//! directory rules: a by-name `mod x;` loads `<dir>/x.rs` or `<dir>/x/mod.rs`,
//! where `<dir>` is the directory its declarer's children sit in; a `#[path]`
//! is relative to the directory of the file declaring it, and the file it names
//! owns the directory it sits in for its own `mod`s. Every declaration is
//! followed wherever its `#[path]` leads, so a route file loaded through a
//! `#[path]` declared in a file of another directory (`routes/admin.rs`
//! declaring `#[path = "helpers/x.rs"] mod x;`) is the module `routes::admin::x`,
//! whatever its own directory is. Inline `mod name { .. }` blocks are walked
//! too: their `mod`s load files under `<dir>/name/` (or the `#[path]` of the
//! block), carrying the block's own `#[cfg(..)]` chain. Only `mod` items of a
//! declarer's top level and of such blocks count.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use super::module_scope::{cfg_predicates, sibling_rs_file};
use crate::{file_utils::file_to_segments, schema_macro::file_cache};

/// One `mod` declaration on the way to a route file: the Rust name it is
/// declared under and its own `#[cfg(..)]` predicates.
#[derive(Clone)]
struct Step {
    name: String,
    cfgs: Vec<String>,
}

/// The `mod` chains that load each route file of a folder, by the route file's
/// segments relative to the folder. A chain is every declaration from the
/// folder's declarers down to the file, in order; a file several declarations
/// load (cfg-disjoint twins) has several chains.
pub(super) struct ModuleTable {
    by_segments: HashMap<Vec<String>, Vec<Vec<Step>>>,
}

impl ModuleTable {
    /// The table of the route folder `folder_path`.
    pub(super) fn new(folder_path: &Path) -> Self {
        let folder = fold_dots(folder_path);
        let mut walk = Walk {
            folder: &folder,
            by_segments: HashMap::new(),
            chain: Vec::new(),
            on_path: HashSet::new(),
        };
        walk.declarer(&folder.join("mod.rs"), &folder);
        walk.declarer(&sibling_rs_file(&folder), &folder);
        Self {
            by_segments: walk.by_segments,
        }
    }

    /// The names the route file with `segments` (relative to the folder) is
    /// called through: each module as its declaration names it (a `#[path]`
    /// module by its declaration), a segment no declaration loads keeping its
    /// own name.
    pub(super) fn declared_segments(&self, segments: &[String]) -> Vec<String> {
        self.resolve(segments).0
    }

    /// The `#[cfg(..)]` predicates (token text, ANDed) under which the route
    /// file with `segments` is part of the crate: none when an ungated
    /// declaration loads it.
    pub(super) fn cfg_predicates(&self, segments: &[String]) -> Vec<String> {
        self.resolve(segments).1
    }

    /// The two call paths (`a::b`) of the route file `segments` when its
    /// cfg-gated declarations name it differently: `#[cfg(unix)]
    /// #[path = "users_impl.rs"] mod users;` beside `#[cfg(not(unix))] mod
    /// users_impl;` loads one file under two names, and no single call path is
    /// right on both platforms. An ungated declaration loads the file in every
    /// build under its own name, so it is the call path and nothing conflicts
    /// (`pub mod shared;` beside an ungated `#[path]` twin).
    pub(super) fn name_conflict(&self, segments: &[String]) -> Option<(String, String)> {
        let (_, chains) = self.loaded_chains(segments)?;
        if chains.iter().any(|chain| ungated(chain)) {
            return None;
        }
        let call_path = |chain: &[Step]| {
            chain
                .iter()
                .map(|step| step.name.as_str())
                .collect::<Vec<_>>()
                .join("::")
        };
        let first = call_path(chains.first()?);
        let other = chains
            .iter()
            .map(|chain| call_path(chain))
            .find(|path| *path != first)?;
        Some((first, other))
    }

    /// A hash of the whole table: the route-cache key covers it, so an edit to
    /// a declaration outside the folder (a `#[path]` declarer under `src/`)
    /// that changes a route file's module chain or predicates invalidates the
    /// cached router, even though no fingerprinted file changed.
    pub(super) fn digest(&self) -> u64 {
        let mut entries: Vec<(&Vec<String>, &Vec<Vec<Step>>)> = self.by_segments.iter().collect();
        entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let mut hasher = DefaultHasher::new();
        for (segments, chains) in entries {
            segments.hash(&mut hasher);
            chains.len().hash(&mut hasher);
            for chain in chains {
                chain.len().hash(&mut hasher);
                for step in chain {
                    step.name.hash(&mut hasher);
                    step.cfgs.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }

    /// The longest leading run of `segments` some declaration loads (the file
    /// itself, or the module above it) with the chains loading it.
    fn loaded_chains(&self, segments: &[String]) -> Option<(usize, &Vec<Vec<Step>>)> {
        (1..=segments.len()).rev().find_map(|depth| {
            self.by_segments
                .get(&segments[..depth])
                .map(|chains| (depth, chains))
        })
    }

    /// The call-path names and the predicates of the route file `segments`.
    fn resolve(&self, segments: &[String]) -> (Vec<String>, Vec<String>) {
        // The rest of `segments` past the loaded run keep their own names and
        // carry no predicates.
        let Some((depth, chains)) = self.loaded_chains(segments) else {
            return (segments.to_vec(), Vec::new());
        };
        // The call path is an ungated chain's name when one exists (it loads
        // the file in every build), else the first chain's.
        let chain = chains
            .iter()
            .find(|chain| ungated(chain))
            .unwrap_or(&chains[0]);
        let mut names: Vec<String> = chain.iter().map(|step| step.name.clone()).collect();
        names.extend_from_slice(&segments[depth..]);
        let per_chain: Vec<Vec<String>> = chains
            .iter()
            .map(|chain| {
                chain
                    .iter()
                    .flat_map(|step| step.cfgs.iter().cloned())
                    .collect()
            })
            .collect();
        (names, declared_cfg(&per_chain))
    }
}

/// The walk of [`ModuleTable::new`].
struct Walk<'a> {
    folder: &'a Path,
    by_segments: HashMap<Vec<String>, Vec<Vec<Step>>>,
    /// The declarations from the folder's declarers down to the one being read.
    chain: Vec<Step>,
    /// The declaring files on the current chain: a `#[path]` cycle ends there.
    on_path: HashSet<PathBuf>,
}

impl Walk<'_> {
    /// Reads the `mod`s `declarer` declares, whose children sit in `children_dir`.
    fn declarer(&mut self, declarer: &Path, children_dir: &Path) {
        if !self.on_path.insert(declarer.to_path_buf()) {
            return;
        }
        let dir = declarer.parent().unwrap_or(children_dir);
        for module in declared_mods(declarer) {
            self.module(dir, children_dir, &module);
        }
        self.on_path.remove(declarer);
    }

    /// One `mod` of a declarer whose `mod`s sit in `dir` (a `#[path]` is
    /// relative to it) and whose by-name children sit in `children_dir`: the
    /// chain through it is recorded under the segments of the route file it
    /// loads, then the files it loads are read. A `#[path]` that names no
    /// route file segment (`mod.rs` of the folder, or an empty value) loads the
    /// module by its own name, as without one.
    fn module(&mut self, dir: &Path, children_dir: &Path, module: &DeclaredMod) {
        if let Some(content) = &module.inline {
            // An inline block loads no file itself: its `mod`s sit in the
            // directory it names (`#[path]`, else `<children_dir>/<name>`).
            let inline_dir = module
                .path
                .as_deref()
                .filter(|path| !path.is_empty())
                .map_or_else(
                    || children_dir.join(&module.name),
                    |path| fold_dots(&dir.join(path)),
                );
            self.chain.push(Step {
                name: module.name.clone(),
                cfgs: module.cfgs.clone(),
            });
            for inner in content {
                self.module(&inline_dir, &inline_dir, inner);
            }
            self.chain.pop();
            return;
        }
        let by_path = module
            .path
            .as_deref()
            .filter(|path| !path.is_empty())
            .map(|path| fold_dots(&dir.join(path)))
            .filter(|file| self.segments(file) != Some(Vec::new()));
        let (segments, loads) = by_path.map_or_else(
            || self.by_name(children_dir, &module.name),
            |file| self.loaded_file(file),
        );
        self.chain.push(Step {
            name: module.name.clone(),
            cfgs: module.cfgs.clone(),
        });
        if let Some(segments) = segments.filter(|segments| !segments.is_empty()) {
            self.by_segments
                .entry(segments)
                .or_default()
                .push(self.chain.clone());
        }
        for (file, children) in loads {
            self.declarer(&file, &children);
        }
        self.chain.pop();
    }

    /// The route-file segments and the files a `mod` loads by a `#[path]` of
    /// `file`, each with the directory its own `mod`s sit in.
    fn loaded_file(&self, file: PathBuf) -> (Option<Vec<String>>, Vec<(PathBuf, PathBuf)>) {
        let children = file.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        (self.segments(&file), vec![(file, children)])
    }

    /// The by-name `mod name;` of a declarer whose children sit in
    /// `children_dir`: the route-file segments and both files it may load
    /// (`<name>.rs` or `<name>/mod.rs`, each with the directory `<name>/`).
    fn by_name(
        &self,
        children_dir: &Path,
        name: &str,
    ) -> (Option<Vec<String>>, Vec<(PathBuf, PathBuf)>) {
        let base = children_dir.join(name);
        (
            self.segments(&base),
            vec![
                (sibling_rs_file(&base), base.clone()),
                (base.join("mod.rs"), base),
            ],
        )
    }

    /// The route-file segments of `file` relative to the folder, `None` when it
    /// lies outside the folder (it is never a route file).
    fn segments(&self, file: &Path) -> Option<Vec<String>> {
        file.strip_prefix(self.folder)
            .ok()
            .map(|relative| file_to_segments(relative, Path::new("")))
    }
}

/// A chain no `#[cfg(..)]` gates: it loads the file in every build.
fn ungated(chain: &[Step]) -> bool {
    chain.iter().all(|step| step.cfgs.is_empty())
}

/// The predicates under which the `mod` declarations of one module load it,
/// one list per chain (each chain's predicates are ANDed). An ungated chain
/// loads the module in every build, so nothing gates it. Cfg-disjoint twins
/// are ORed (only one of them is compiled per build): `#[cfg(unix)] mod m;`
/// beside `#[cfg(not(unix))] mod m;` is `any(all(unix), all(not(unix)))`.
fn declared_cfg(chains: &[Vec<String>]) -> Vec<String> {
    if chains.iter().any(Vec::is_empty) {
        return Vec::new();
    }
    match chains {
        [single] => single.clone(),
        many => {
            let twins: Vec<String> = many
                .iter()
                .map(|cfgs| format!("all({})", cfgs.join(", ")))
                .collect();
            vec![format!("any({})", twins.join(", "))]
        }
    }
}

/// One `mod` a declarer makes: the Rust name it is declared under, its
/// `#[path]` literal (`None` without one), its `#[cfg(..)]` predicates and,
/// for an inline `mod name { .. }` block, the `mod`s inside it.
#[derive(Clone)]
struct DeclaredMod {
    name: String,
    path: Option<String>,
    cfgs: Vec<String>,
    inline: Option<Vec<Self>>,
}

/// A declarer's top-level `mod`s (none when the file does not parse).
#[derive(Clone, Default)]
struct Declared(Vec<DeclaredMod>);

/// The top-level `mod`s of `declarer`, read once per file version
/// (`file_cache::derived`). A file that does not exist declares none.
fn declared_mods(declarer: &Path) -> Vec<DeclaredMod> {
    file_cache::derived(declarer, |content| {
        Declared(
            syn::parse_file(content)
                .map(|file| declared_items(&file.items))
                .unwrap_or_default(),
        )
    })
    .map(|declared| declared.0.clone())
    .unwrap_or_default()
}

/// The `mod` items among `items`, each with the `mod`s of its inline block.
fn declared_items(items: &[syn::Item]) -> Vec<DeclaredMod> {
    items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) => Some(DeclaredMod {
                name: crate::parser::strip_raw_prefix_owned(module.ident.to_string()),
                path: declared_path(module),
                cfgs: cfg_predicates(&module.attrs),
                inline: module
                    .content
                    .as_ref()
                    .map(|(_, content)| declared_items(content)),
            }),
            _ => None,
        })
        .collect()
}

/// The literal `#[path = ".."]` of a `mod` declaration, if it has one.
fn declared_path(module: &syn::ItemMod) -> Option<String> {
    module
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("path"))
        .and_then(|attr| attr.meta.require_name_value().ok())
        .and_then(|named| match &named.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(value),
                ..
            }) => Some(value.value()),
            _ => None,
        })
}

/// `path` with its `.` and `..` components folded away (no filesystem access).
fn fold_dots(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// Writes `files` (relative path, content) under a fresh temp dir.
    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
        file_cache::bump_epoch();
        dir
    }

    fn segs(parts: &[&str]) -> Vec<String> {
        parts.iter().map(ToString::to_string).collect()
    }

    #[rstest]
    #[case::plain_module("mod admin;", &["admin"], &[])]
    #[case::gated_module("#[cfg(feature = \"admin\")] mod admin;", &["admin"], &["feature = \"admin\""])]
    #[case::two_cfgs("#[cfg(unix)] #[cfg(test)] #[cfg] mod admin;", &["admin"], &["unix", "test"])]
    #[case::other_module_gated("#[cfg(test)] mod tests; mod admin;", &["admin"], &[])]
    #[case::folder_root(r#"#[cfg(feature = "x")] mod admin;"#, &[], &[])]
    #[case::raw_keyword_module("#[cfg(unix)] pub mod r#type;", &["type"], &["unix"])]
    // A `#[path]` module loads the file its path names, not its own name.
    #[case::path_attribute(
        "#[cfg(feature = \"admin\")] #[path = \"admin_impl.rs\"] mod admin;",
        &["admin_impl"],
        &["feature = \"admin\""]
    )]
    #[case::path_attribute_by_own_name(
        "#[cfg(feature = \"admin\")] #[path = \"admin_impl.rs\"] mod admin;",
        &["admin"],
        &[]
    )]
    #[case::path_mod_rs("#[cfg(unix)] #[path = \"admin/mod.rs\"] mod admin;", &["admin"], &["unix"])]
    #[case::path_mod_rs_bare("#[cfg(unix)] #[path = \"mod.rs\"] mod admin;", &["admin"], &["unix"])]
    #[case::path_without_stem("#[cfg(unix)] #[path = \"\"] mod admin;", &["admin"], &["unix"])]
    #[case::non_literal_path("#[cfg(unix)] #[path = concat!(\"a\", \".rs\")] mod admin;", &["admin"], &["unix"])]
    #[case::undeclared_module("mod other;", &["admin"], &[])]
    // A `mod` inside an inline block loads `<dir>/<block>/<name>` (round-40
    // MR1-L1), under the block's own cfg chain as well as its own.
    #[case::inline_block_module("mod v1 { #[cfg(unix)] pub mod users; }", &["v1", "users"], &["unix"])]
    #[case::inline_block_gated("#[cfg(unix)] mod v1 { pub mod users; }", &["v1", "users"], &["unix"])]
    #[case::nested_inline_blocks(
        "#[cfg(unix)] mod v1 { #[cfg(feature = \"x\")] mod v2 { #[cfg(test)] pub mod users; } }",
        &["v1", "v2", "users"],
        &["unix", "feature = \"x\"", "test"]
    )]
    #[case::inline_block_path(
        "mod v1 { #[cfg(unix)] #[path = \"impl/users.rs\"] pub mod users; }",
        &["v1", "impl", "users"],
        &["unix"]
    )]
    #[case::inline_block_path_attribute(
        "#[path = \"base\"] mod v1 { pub mod users; }",
        &["base", "users"],
        &[]
    )]
    // A `#[path]` file in a subdirectory is one module loading both
    // directories (round-33 R33-M1), so its gate applies at the root.
    #[case::path_subdirectory(
        "#[cfg(feature = \"admin\")] #[path = \"beside/support.rs\"] pub mod support;",
        &["beside", "support"],
        &["feature = \"admin\""]
    )]
    #[case::path_mod_rs_subdirectory(
        "#[cfg(unix)] #[path = \"x/admin/mod.rs\"] pub mod nested;",
        &["x", "admin"],
        &["unix"]
    )]
    // The declaration loading the longest run of segments wins over a `mod`
    // of the first segment alone.
    #[case::longest_declaration_wins(
        "#[cfg(unix)] pub mod beside;\n#[cfg(feature = \"x\")] #[path = \"beside/support.rs\"] pub mod support;",
        &["beside", "support"],
        &["feature = \"x\""]
    )]
    // Cfg-disjoint twins load the module under either predicate (round-30 M1):
    // ANDing them would never load it.
    #[case::cfg_twins(
        "#[cfg(unix)] pub mod admin;\n#[cfg(not(unix))] pub mod admin;",
        &["admin"],
        &["any(all(unix), all(not (unix)))"]
    )]
    #[case::multi_cfg_twins(
        "#[cfg(unix)] #[cfg(test)] mod admin; #[cfg(windows)] mod admin;",
        &["admin"],
        &["any(all(unix, test), all(windows))"]
    )]
    // An ungated twin loads the module in every build: nothing gates it.
    #[case::ungated_twin("#[cfg(unix)] pub mod admin;\npub mod admin;", &["admin"], &[])]
    fn module_declaration_cfg(
        #[case] mod_rs: &str,
        #[case] segments: &[&str],
        #[case] expected: &[&str],
    ) {
        let dir = tree(&[("mod.rs", mod_rs)]);
        assert_eq!(
            ModuleTable::new(dir.path()).cfg_predicates(&segs(segments)),
            expected
        );
    }

    /// The call path names a `#[path]` module by its declaration; a file no
    /// declaration names keeps its own name.
    #[rstest]
    #[case::path_module(&["admin_impl"], &["admin"])]
    #[case::own_name(&["users"], &["users"])]
    #[case::undeclared(&["stray"], &["stray"])]
    #[case::path_subdirectory(&["beside", "support"], &["support"])]
    #[case::path_mod_rs_subdirectory(&["x", "admin"], &["nested"])]
    #[case::dot_prefixed_path(&["dot", "item"], &["dotted"])]
    #[case::empty_path_keeps_own_name(&["empty"], &["empty"])]
    fn declared_module_names(#[case] segments: &[&str], #[case] expected: &[&str]) {
        let dir = tree(&[(
            "mod.rs",
            "#[path = \"admin_impl.rs\"] pub mod admin; pub mod users;\n\
             #[path = \"beside/support.rs\"] pub mod support;\n\
             #[path = \"x/admin/mod.rs\"] pub mod nested;\n\
             #[path = \"./dot/item.rs\"] pub mod dotted;\n\
             #[path = \"\"] pub mod empty;",
        )]);
        assert_eq!(
            ModuleTable::new(dir.path()).declared_segments(&segs(segments)),
            expected
        );
    }

    #[test]
    fn folder_with_dot_declared_in_sibling_file_with_full_name() {
        // `api.v2` is declared by `api.v2.rs`; `with_extension` would look
        // for `api.rs`.
        let dir = tree(&[
            ("api.v2.rs", "#[cfg(feature = \"admin\")] mod admin;"),
            ("api.rs", "mod admin;"),
            ("api.v2/.keep", ""),
        ]);
        let folder = dir.path().join("api.v2");
        assert_eq!(
            ModuleTable::new(&folder).cfg_predicates(&segs(&["admin"])),
            ["feature = \"admin\""]
        );
    }

    #[test]
    fn nested_module_declared_in_sibling_file() {
        let dir = tree(&[
            ("routes/mod.rs", "pub mod admin;"),
            ("routes/admin.rs", "#[cfg(feature = \"x\")] pub mod users;"),
        ]);
        let routes = dir.path().join("routes");
        let table = ModuleTable::new(&routes);
        assert_eq!(
            table.cfg_predicates(&segs(&["admin", "users"])),
            ["feature = \"x\""]
        );
        assert_eq!(
            table.cfg_predicates(&segs(&["admin"])),
            Vec::<String>::new()
        );
    }

    /// A `#[path]` on a `mod` in `src/routes.rs` (not `routes/mod.rs`) is
    /// relative to `src/`, the directory of that file: `routes/admin_impl.rs`
    /// is the route file `src/routes/admin_impl.rs`, loaded as `routes::admin`.
    #[test]
    fn path_attribute_in_sibling_file_is_relative_to_its_directory() {
        let dir = tree(&[
            (
                "routes.rs",
                "#[cfg(feature = \"admin\")] #[path = \"routes/admin_impl.rs\"] mod admin;",
            ),
            ("routes/admin_impl.rs", ""),
        ]);
        let routes = dir.path().join("routes");
        let table = ModuleTable::new(&routes);
        let segments = segs(&["admin_impl"]);
        assert_eq!(table.declared_segments(&segments), ["admin"]);
        assert_eq!(table.cfg_predicates(&segments), ["feature = \"admin\""]);
    }

    /// `#[path]` components fold lexically: `routes/../routes/x.rs` from
    /// `src/` is `src/routes/x.rs`.
    #[test]
    fn path_attribute_in_sibling_file_folds_parent_components() {
        let dir = tree(&[
            (
                "routes.rs",
                "#[cfg(unix)] #[path = \"routes/../routes/x.rs\"] mod m;",
            ),
            ("routes/x.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["x"]);
        assert_eq!(table.declared_segments(&segments), ["m"]);
        assert_eq!(table.cfg_predicates(&segments), ["unix"]);
    }

    /// `#[path = "x.rs"]` in `src/routes.rs` loads `src/x.rs`, outside the
    /// route folder: it never gates the walked `routes/x.rs`.
    #[test]
    fn path_attribute_in_sibling_file_outside_folder_gates_nothing() {
        let dir = tree(&[
            ("routes.rs", "#[cfg(unix)] #[path = \"x.rs\"] mod m;"),
            ("routes/x.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["x"]);
        assert_eq!(table.declared_segments(&segments), ["x"]);
        assert_eq!(table.cfg_predicates(&segments), Vec::<String>::new());
    }

    /// `#[path = "legacy/users.rs"]` in `src/routes.rs` loads `src/legacy/users.rs`,
    /// not `routes/legacy/users.rs`: the gated module never names the route file.
    #[test]
    fn path_attribute_in_sibling_file_is_not_relative_to_the_folder() {
        let dir = tree(&[
            (
                "routes.rs",
                "#[cfg(unix)] #[path = \"legacy/users.rs\"] mod users;",
            ),
            ("routes/legacy/users.rs", ""),
            ("legacy/users.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["legacy", "users"]);
        assert_eq!(table.declared_segments(&segments), ["legacy", "users"]);
        assert_eq!(table.cfg_predicates(&segments), Vec::<String>::new());
    }

    /// `#[path = "helpers/x.rs"]` declared in `routes/admin.rs` loads
    /// `routes/helpers/x.rs` (relative to the declaring file's directory), so
    /// the route file is the module `routes::admin::x` and its gate applies.
    #[test]
    fn path_declared_in_a_directory_file_loads_a_file_beside_it() {
        let dir = tree(&[
            ("routes/mod.rs", "pub mod admin;"),
            (
                "routes/admin.rs",
                "#[cfg(feature = \"x\")] #[path = \"helpers/x.rs\"] pub mod x;",
            ),
            ("routes/helpers/x.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["helpers", "x"]);
        assert_eq!(table.declared_segments(&segments), ["admin", "x"]);
        assert_eq!(table.cfg_predicates(&segments), ["feature = \"x\""]);
    }

    /// The `<dir>/<dir>/..` shape: `routes/admin.rs` declares
    /// `#[path = "admin/extra.rs"] mod extra;`, which is `routes/admin/extra.rs`.
    #[test]
    fn path_declared_in_a_directory_file_into_its_own_directory() {
        let dir = tree(&[
            ("routes/mod.rs", "pub mod admin;"),
            (
                "routes/admin.rs",
                "#[cfg(unix)] #[path = \"admin/extra.rs\"] pub mod extra;",
            ),
            ("routes/admin/extra.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["admin", "extra"]);
        assert_eq!(table.declared_segments(&segments), ["admin", "extra"]);
        assert_eq!(table.cfg_predicates(&segments), ["unix"]);
    }

    /// A `#[path]` loop (`routes/admin.rs` declares itself) ends at the file
    /// already on the walk, and the first chain keeps its names.
    #[test]
    fn path_cycle_through_a_directory_file_terminates() {
        let dir = tree(&[
            ("routes/mod.rs", "pub mod admin;"),
            ("routes/admin.rs", "#[path = \"admin.rs\"] pub mod again;"),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["admin"]);
        assert_eq!(table.declared_segments(&segments), ["admin"]);
        assert_eq!(table.cfg_predicates(&segments), Vec::<String>::new());
    }

    /// A `#[path]` in `routes/admin/mod.rs` that reaches up to
    /// `routes/helpers/x.rs` keeps the gate of the `admin` declaration above
    /// it, and the route file is called as `routes::admin::x`.
    #[test]
    fn route_file_loaded_through_nested_path_keeps_its_module_chain() {
        let dir = tree(&[
            ("routes/mod.rs", "#[cfg(unix)] pub mod admin;"),
            (
                "routes/admin/mod.rs",
                "#[path = \"../helpers/x.rs\"] pub mod x;",
            ),
            ("routes/helpers/x.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        let segments = segs(&["helpers", "x"]);
        assert_eq!(table.declared_segments(&segments), ["admin", "x"]);
        assert_eq!(table.cfg_predicates(&segments), ["unix"]);
    }

    /// A declarer outside the folder (`routes.rs` loads `shared/api.rs`, which
    /// gates `routes::admin`) decides the route file's predicates: the digest
    /// changes with it, so a cached router built without the gate is not
    /// reused (round-36 MR1-L1).
    #[test]
    fn digest_follows_a_declarer_outside_the_folder() {
        let tree_with = |cfg: &str| {
            let api = format!("{cfg} #[path = \"../routes/admin.rs\"] pub mod admin;");
            tree(&[
                ("routes.rs", "#[path = \"shared/api.rs\"] mod api;"),
                ("shared/api.rs", api.as_str()),
                ("routes/admin.rs", ""),
            ])
        };
        let digest =
            |dir: &tempfile::TempDir| ModuleTable::new(&dir.path().join("routes")).digest();
        let plain = tree_with("");
        let gated = tree_with("#[cfg(feature = \"admin\")]");
        assert_ne!(digest(&plain), digest(&gated));
        assert_eq!(
            digest(&gated),
            digest(&tree_with("#[cfg(feature = \"admin\")]"))
        );
    }

    /// Cfg-disjoint declarations that name one route file differently have no
    /// single call path; the same name under both predicates has one
    /// (round-36 MR1-L2).
    #[test]
    fn twins_naming_one_file_differently_conflict() {
        let dir = tree(&[
            (
                "routes/mod.rs",
                "#[cfg(unix)] #[path = \"users_impl.rs\"] pub mod users;\n#[cfg(not(unix))] pub mod users_impl;",
            ),
            ("routes/users_impl.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        assert_eq!(
            table.name_conflict(&segs(&["users_impl"])),
            Some(("users".to_string(), "users_impl".to_string()))
        );
        assert_eq!(table.name_conflict(&segs(&["nope"])), None);
        let same = tree(&[(
            "routes/mod.rs",
            "#[cfg(unix)] pub mod admin;\n#[cfg(not(unix))] pub mod admin;",
        )]);
        let table = ModuleTable::new(&same.path().join("routes"));
        assert_eq!(table.name_conflict(&segs(&["admin"])), None);
    }

    /// An ungated declaration loads the route file in every build, so a
    /// differently named ungated twin (a route-free helper loaded twice) is no
    /// conflict and the first ungated name is the call path, in either order
    /// (round-37 MR0-M1).
    #[rstest]
    #[case::plain_first(
        "pub mod shared;
#[path = \"shared.rs\"] pub mod shared_alias;",
        "shared"
    )]
    #[case::path_first(
        "#[path = \"shared.rs\"] pub mod shared_alias;
pub mod shared;",
        "shared_alias"
    )]
    fn ungated_twins_are_not_a_conflict(#[case] mod_rs: &str, #[case] call_name: &str) {
        let dir = tree(&[("routes/mod.rs", mod_rs), ("routes/shared.rs", "")]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        assert_eq!(table.name_conflict(&segs(&["shared"])), None);
        assert_eq!(table.declared_segments(&segs(&["shared"])), [call_name]);
        assert_eq!(
            table.cfg_predicates(&segs(&["shared"])),
            Vec::<String>::new()
        );
    }

    /// A gated twin beside an ungated module: the ungated name is the call path
    /// whichever is declared first, and the file is not gated by the twin
    /// (round-37 MR1-M1).
    #[rstest]
    #[case::gated_twin_first(
        "#[cfg(test)] #[path = \"admin.rs\"] mod admin_twin;
pub mod admin;"
    )]
    #[case::gated_twin_last(
        "pub mod admin;
#[cfg(test)] #[path = \"admin.rs\"] mod admin_twin;"
    )]
    fn gated_twin_beside_ungated_module_keeps_the_ungated_name(#[case] mod_rs: &str) {
        let dir = tree(&[("routes/mod.rs", mod_rs), ("routes/admin.rs", "")]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        assert_eq!(table.name_conflict(&segs(&["admin"])), None);
        assert_eq!(table.declared_segments(&segs(&["admin"])), ["admin"]);
        assert_eq!(
            table.cfg_predicates(&segs(&["admin"])),
            Vec::<String>::new()
        );
    }

    /// Only genuinely gated differing names conflict: the twin under
    /// `#[cfg(test)]` beside the same file under `#[cfg(not(test))]` (round-37
    /// MR1-M1, the disjoint pair keeps erroring).
    #[test]
    fn gated_differing_names_without_an_ungated_chain_still_conflict() {
        let dir = tree(&[
            (
                "routes/mod.rs",
                "#[cfg(test)] #[path = \"admin.rs\"] pub mod admin_twin;
#[cfg(not(test))] pub mod admin;",
            ),
            ("routes/admin.rs", ""),
        ]);
        let table = ModuleTable::new(&dir.path().join("routes"));
        assert_eq!(
            table.name_conflict(&segs(&["admin"])),
            Some(("admin_twin".to_string(), "admin".to_string()))
        );
    }

    /// The collector refuses such a folder with an error naming both paths,
    /// instead of routing one platform to a module that does not exist.
    #[test]
    fn twin_names_for_a_route_file_are_a_compile_error() {
        let dir = tree(&[
            (
                "routes/mod.rs",
                "#[cfg(unix)] #[path = \"users_impl.rs\"] pub mod users;\n#[cfg(not(unix))] pub mod users_impl;",
            ),
            (
                "routes/users_impl.rs",
                "#[route(get)] pub async fn list() {}\n",
            ),
        ]);
        let Err(error) =
            crate::collector::collect_metadata(&dir.path().join("routes"), "routes", &[])
        else {
            panic!("twin declarations naming one file differently must be rejected");
        };
        assert!(
            error.to_string().contains("`users` and `users_impl`"),
            "{error}"
        );
    }

    #[test]
    fn fold_dots_folds_current_and_parent_dirs() {
        assert_eq!(
            fold_dots(Path::new("./src/routes/../api/x.rs")),
            Path::new("src/api/x.rs")
        );
    }

    /// `#[cfg_attr(p, cfg(q))]` on a `mod` declaration gates the module it
    /// loads (round-32 macrorest-1).
    #[rstest]
    #[case::wrapped(
        "#[cfg_attr(feature = \"admin\", cfg(unix))] pub mod admin;",
        &["any(not(feature=\"admin\"),unix)"]
    )]
    #[case::statically_false("#[cfg_attr(any(), cfg(unix))] pub mod admin;", &[])]
    fn cfg_attr_module_declaration_cfg(#[case] mod_rs: &str, #[case] expected: &[&str]) {
        let dir = tree(&[("mod.rs", mod_rs)]);
        let predicates: Vec<String> = ModuleTable::new(dir.path())
            .cfg_predicates(&segs(&["admin"]))
            .into_iter()
            .map(|predicate| predicate.replace(' ', ""))
            .collect();
        assert_eq!(predicates, expected);
    }

    /// The end-to-end view: a route file loaded through `routes/admin.rs`'s
    /// `#[path]` is collected as the gated module `routes::admin::x`, not as
    /// the unresolvable `routes::helpers::x`.
    #[test]
    fn collected_route_in_a_path_loaded_file_is_called_through_its_chain() {
        let dir = tree(&[
            ("routes/mod.rs", "pub mod admin;"),
            (
                "routes/admin.rs",
                "#[cfg(feature = \"x\")] #[path = \"helpers/x.rs\"] pub mod x;",
            ),
            ("routes/helpers/x.rs", "#[route(get)] pub async fn h() {}\n"),
        ]);
        let routes = dir.path().join("routes");
        let (metadata, _) = crate::collector::collect_metadata(&routes, "routes", &[]).unwrap();
        assert!(metadata.routes.is_empty());
        let gated: Vec<(&str, &str, &[String])> = metadata
            .cfg_routes
            .iter()
            .map(|route| {
                (
                    route.module_path.as_str(),
                    route.path.as_str(),
                    route.cfg.as_slice(),
                )
            })
            .collect();
        assert_eq!(
            gated,
            [(
                "routes::admin::x",
                "/helpers/x",
                &["feature = \"x\"".to_string()][..]
            )]
        );
    }
}
