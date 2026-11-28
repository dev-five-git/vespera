//! The `use` items of one module of the cron scan: which path a `#[cron]`
//! attribute names there (a bare `cron`, a local alias of vespera's `cron`, or
//! another crate's), and which aliases a `use super::*` / `use crate::*` glob
//! brings down from the enclosing module ([`CronImports::inheriting`]).

/// The `use` items of one module that decide what a bare `#[cron]` in it
/// names (see [`CronImports::foreign`]), and the local names vespera's `cron`
/// and the vespera crate are bound to in it.
#[derive(Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct CronImports {
    /// `use other::cron;`, `use other::{cron};`, `use other::job as cron;`.
    explicit_foreign: bool,
    /// `use vespera::cron;` (or `crate` / `self` / `super` rooted).
    explicit_vespera: bool,
    /// `use other::*;`.
    foreign_glob: bool,
    /// `use super::cron;` / `use crate::cron;`: the bare `cron` this module
    /// names is the parent's (`Some(false)`) or the crate root's (`Some(true)`),
    /// whatever its foreign state is there.
    inherited_cron: Option<bool>,
    /// Single names a `use super::X;` / `use crate::X as Y;` (or a grouped form)
    /// imports from the parent or the crate root (never `cron` itself, see
    /// [`CronImports::inherited_cron`]). They are resolved against that module's
    /// aliases in [`CronImports::inheriting`].
    inherited_names: Vec<InheritedName>,
    /// `use super::*;`: the names of the enclosing module (its own `cron`
    /// import) reach this module through the glob.
    super_glob: bool,
    /// `use crate::*;`: the names of the crate root reach this module through
    /// the glob (not those of the enclosing module).
    crate_glob: bool,
    /// Every name a `use` item of this module binds (`use other::job;`,
    /// `use vespera::cron as job;`): it shadows an inherited alias of the same
    /// name, since an explicit import beats a glob.
    bound: Vec<String>,
    /// Local names of `vespera::cron` (`use vespera::cron as job;`), this
    /// module's own and those inherited through its globs.
    cron_aliases: Vec<String>,
    /// Local names of the vespera crate (`use vespera as v;`), own and inherited.
    crate_aliases: Vec<String>,
}

/// One single-name `use super::X as Y;` / `use crate::X;` import.
#[derive(Clone)]
struct InheritedName {
    /// `X`, the name in the parent or the crate root.
    source: String,
    /// `Y` (`X` without a rename), the name this module binds.
    local: String,
    /// Whether it comes from the crate root (`crate::`) rather than the parent.
    from_root: bool,
}

/// A module's effective `use` state, as a glob or an explicit import of a child
/// module inherits it: whether its bare `cron` is another crate's (see
/// [`CronImports::foreign`]) and the imports in effect.
#[derive(Clone, Copy)]
pub(super) struct Inherited<'a> {
    pub(super) foreign: bool,
    pub(super) imports: &'a CronImports,
}

impl CronImports {
    /// Whether a bare `#[cron]` in this module is another crate's attribute.
    /// An explicit `cron` import shadows a glob, so it decides: vespera's when
    /// it is rooted at `vespera`, `crate`, `self` or `super`, whatever the
    /// globs bring in. Without one, a foreign glob makes the bare form foreign,
    /// and so does a `use super::*;` glob when the enclosing module's bare
    /// `cron` is foreign (`parent`), or a `use crate::*;` glob when the crate
    /// root's is (`root`).
    pub(super) fn foreign(&self, parent: bool, root: bool) -> bool {
        if self.explicit_foreign {
            return true;
        }
        if self.explicit_vespera {
            return false;
        }
        match self.inherited_cron {
            Some(true) => root,
            Some(false) => parent,
            None => self.foreign_glob || (self.super_glob && parent) || (self.crate_glob && root),
        }
    }

    /// The imports in effect in this module: its own, plus the aliases its
    /// `use super::*;` glob brings from `parent` (the enclosing module's
    /// effective imports) and its `use crate::*;` glob brings from `root` (the
    /// crate root's effective imports), except a name this module's own `use`
    /// items bind. A module without such a glob sees only its own.
    pub(super) fn inheriting(&self, parent: Inherited<'_>, root: Inherited<'_>) -> Self {
        let mut effective = self.clone();
        for (glob, source) in [(self.super_glob, parent), (self.crate_glob, root)] {
            if glob {
                extend_unbound(
                    &mut effective.cron_aliases,
                    &source.imports.cron_aliases,
                    &self.bound,
                );
                extend_unbound(
                    &mut effective.crate_aliases,
                    &source.imports.crate_aliases,
                    &self.bound,
                );
            }
        }
        // An explicit single-name import is not shadowed by a glob: `use
        // super::job;` binds the parent's `job` alias of vespera's `cron`
        // (round-45 R45-MR5-M1), and `use super::cron as job;` binds the
        // parent's bare `cron` under that name when it is vespera's.
        for name in &self.inherited_names {
            let source = if name.from_root { root } else { parent };
            if name.source == "cron" {
                if !source.foreign && name.local != "cron" {
                    push_unique(&mut effective.cron_aliases, &name.local);
                }
            } else {
                if source.imports.cron_aliases.contains(&name.source) {
                    push_unique(&mut effective.cron_aliases, &name.local);
                }
                if source.imports.crate_aliases.contains(&name.source) {
                    push_unique(&mut effective.crate_aliases, &name.local);
                }
            }
        }
        effective
    }

    /// Whether a path spelled `segments` names vespera's `cron` here: `cron`
    /// (or an alias of it), or `vespera::cron` (or `<alias of the vespera
    /// crate>::cron`). An alias is recognised only where this module's `use`
    /// items (or the globs it inherits) bind it, so a `job` bound to another
    /// crate's `cron` is not a job.
    pub(super) fn is_cron_segments(&self, segments: &[String]) -> bool {
        match segments {
            [name] => name == "cron" || self.cron_aliases.contains(name),
            [krate, name] => {
                name == "cron" && (krate == "vespera" || self.crate_aliases.contains(krate))
            }
            _ => false,
        }
    }
}

/// Appends each name of `inherited` that `own` lacks and `bound` does not
/// shadow.
fn extend_unbound(own: &mut Vec<String>, inherited: &[String], bound: &[String]) {
    for name in inherited {
        if !bound.contains(name) {
            push_unique(own, name);
        }
    }
}

fn push_unique(names: &mut Vec<String>, name: &str) {
    if !names.iter().any(|known| known == name) {
        names.push(name.to_owned());
    }
}

/// `Some(false)` for a direct child of `super` (`use super::X;`), `Some(true)`
/// for one of `crate` (`use crate::X;`), `None` for anything else.
fn parent_or_root(root: Option<&syn::Ident>, depth: usize) -> Option<bool> {
    match root.filter(|_| depth == 1)?.to_string().as_str() {
        "super" => Some(false),
        "crate" => Some(true),
        _ => None,
    }
}

/// The [`CronImports`] of `items` (the `use` items of one module).
pub(super) fn cron_imports(items: &[syn::Item]) -> CronImports {
    // `depth` counts the path segments above `tree`: 1 is a direct child of
    // the first segment (`vespera::cron as job` or `vespera::{cron as job}`).
    fn collect(
        tree: &syn::UseTree,
        root: Option<&syn::Ident>,
        depth: usize,
        out: &mut CronImports,
    ) {
        let foreign = root.is_some_and(|root| {
            !matches!(
                root.to_string().as_str(),
                "vespera" | "crate" | "self" | "super"
            )
        });
        let bound = match tree {
            syn::UseTree::Name(name) => Some(name.ident.to_string()),
            syn::UseTree::Rename(rename) => Some(rename.rename.to_string()),
            _ => None,
        };
        out.bound.extend(bound);
        // `use super::X;` / `use crate::X;` (a direct child of the parent or the
        // crate root): `Some(from_root)`.
        let from_parent_or_root = parent_or_root(root, depth);
        match tree {
            syn::UseTree::Path(path) => {
                collect(&path.tree, root.or(Some(&path.ident)), depth + 1, out);
            }
            syn::UseTree::Name(name) if name.ident == "cron" && from_parent_or_root.is_some() => {
                out.inherited_cron = from_parent_or_root;
            }
            syn::UseTree::Rename(rename)
                if rename.rename == "cron" && from_parent_or_root.is_some() =>
            {
                if rename.ident == "cron" {
                    out.inherited_cron = from_parent_or_root;
                }
            }
            syn::UseTree::Name(name) if from_parent_or_root.is_some() => {
                out.inherited_names.push(InheritedName {
                    source: name.ident.to_string(),
                    local: name.ident.to_string(),
                    from_root: from_parent_or_root == Some(true),
                });
            }
            syn::UseTree::Rename(rename) if from_parent_or_root.is_some() => {
                out.inherited_names.push(InheritedName {
                    source: rename.ident.to_string(),
                    local: rename.rename.to_string(),
                    from_root: from_parent_or_root == Some(true),
                });
            }
            syn::UseTree::Name(name) if name.ident == "cron" => {
                out.explicit_foreign |= foreign;
                out.explicit_vespera |= root.is_some() && !foreign;
            }
            syn::UseTree::Rename(rename) if rename.rename == "cron" => {
                out.explicit_foreign |= foreign;
                out.explicit_vespera |= root.is_some() && !foreign;
            }
            syn::UseTree::Rename(rename)
                if rename.ident == "cron"
                    && depth == 1
                    && root.is_some_and(|root| root == "vespera") =>
            {
                out.cron_aliases.push(rename.rename.to_string());
            }
            syn::UseTree::Rename(rename) if rename.ident == "vespera" && root.is_none() => {
                out.crate_aliases.push(rename.rename.to_string());
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    collect(tree, root, depth, out);
                }
            }
            // A glob may bring in another crate's `cron`; only vespera's (or
            // this crate's own) globs leave the bare form vespera's. A
            // `super` / `crate` glob also brings in the names of the enclosing
            // module / of the crate root.
            syn::UseTree::Glob(_) => {
                out.foreign_glob |= foreign;
                out.super_glob |= root.is_some_and(|root| root == "super");
                out.crate_glob |= root.is_some_and(|root| root == "crate");
            }
            _ => {}
        }
    }
    let mut imports = CronImports::default();
    for item in items {
        if let syn::Item::Use(item) = item {
            collect(&item.tree, None, 0, &mut imports);
        }
    }
    imports
}
