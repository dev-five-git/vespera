//! The module a file is loaded as, read from the module tree: the reverse
//! of [`super::module_items`], which honours `#[path = ".."]` and inline
//! `mod`s, so the two directions agree on where a `#[path]`-loaded file
//! sits (its place on disk named another module, or the crate root for a
//! file outside the module directory).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{Cursor, descend, descend_all, file_cache, ident_name, open_file};
use crate::schema_macro::crate_layout::{CrateLayout, path_key};

/// The module `file` is declared as in `layout`'s module tree when the
/// module its place on disk names (`disk`, `["crate", ..]`) does not load
/// it: `#[path = "generated/models.rs"] mod models;` in `src/lib.rs` makes
/// `src/generated/models.rs` `crate::models` (not `crate::generated::models`).
/// `None` keeps `disk`: the tree loads the file there, or no module of the
/// tree loads it (no crate root on disk, a file the crate does not use).
/// Memoised per macro invocation.
pub fn declared_module_of(
    layout: &CrateLayout,
    file: &Path,
    disk: &[String],
) -> Option<Vec<String>> {
    type Key = (u64, CrateLayout, String);
    thread_local! {
        static MEMO: RefCell<HashMap<Key, Option<Vec<String>>>> = RefCell::new(HashMap::new());
    }
    if !file.is_file() {
        return None;
    }
    let key: Key = (file_cache::current_epoch(), layout.clone(), path_key(file));
    if let Some(hit) = MEMO.with(|memo| memo.borrow().get(&key).cloned()) {
        return hit;
    }
    let found = find(layout, &key.2, disk);
    MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        memo.retain(|old, _| old.0 == key.0);
        memo.insert(key, found.clone());
    });
    found
}

/// [`declared_module_of`] for the file whose path key is `target`.
fn find(layout: &CrateLayout, target: &str, disk: &[String]) -> Option<Vec<String>> {
    let roots = || layout.roots.iter().filter_map(|root| open_file(root, true));
    // The common case: the disk path is the declared one (no full walk).
    let loads = |cursor: Cursor| {
        descend_all(cursor, disk.get(1..).unwrap_or_default())
            .is_some_and(|found| found.inline.is_empty() && path_key(&found.file) == target)
    };
    if roots().any(loads) {
        return None;
    }
    let mut visited: HashSet<String> = layout.roots.iter().map(|root| path_key(root)).collect();
    roots().find_map(|root| walk(&root, &["crate".to_string()], target, &mut visited))
}

/// The module below `cursor` (the module `module`) whose file is `target`,
/// every module file opened once (`visited`: a `#[path]` cycle ends).
fn walk(
    cursor: &Cursor,
    module: &[String],
    target: &str,
    visited: &mut HashSet<String>,
) -> Option<Vec<String>> {
    let children: Vec<String> = cursor
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(child) => Some(ident_name(&child.ident)),
            _ => None,
        })
        .collect();
    for name in children {
        let Some(child) = descend(cursor.clone(), &name) else {
            continue;
        };
        let mut child_module = module.to_vec();
        child_module.push(name);
        // A module file of its own (an inline `mod` keeps walking its file).
        if child.inline.is_empty() {
            let key = path_key(&child.file);
            if key == target {
                return Some(child_module);
            }
            if !visited.insert(key) {
                continue;
            }
        }
        if let Some(found) = walk(&child, &child_module, target, visited) {
            return Some(found);
        }
    }
    None
}
