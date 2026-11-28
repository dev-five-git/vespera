//! Stored `#[derive(Schema)]` entries whose item has left its file.
//!
//! A long-lived proc-macro server (rust-analyzer) keeps
//! [`super::SCHEMA_STORAGE`] across edits: after a struct is renamed, moved
//! to another file or its file deleted, the old entry stays registered and
//! the next item of that name was rejected with a false duplicate-name error
//! until the server restarted. A clean build never sees such an entry.
//!
//! Each registration remembers its file's fingerprint; a conflicting stored
//! entry is stale when that file has changed since AND no longer holds an
//! item of that name (or is gone). A file that no longer parses mid-edit is
//! not "gone": it may still hold the item, so it keeps the strict error and
//! the variant. An unchanged file — every file of a
//! `cargo build` — keeps the strict duplicate-name error.
//!
//! Staleness is judged in two scopes. A duplicate-name check ([`left_its_file`])
//! asks the whole file: a name still declared anywhere in it is a duplicate
//! (SM0-L1), so an item moved out of its module beside a same-named twin is
//! not "left". A variant ([`left_its_module`], R34-SM-L1) asks its own inline
//! module only: a same-named item of a sibling `mod` does not keep a deleted
//! item alive.
//!
//! A `schema_type!`-generated DTO has no textual declaration. It has left its
//! file only when the invocation naming it at its recorded position is gone
//! (SM0-L2), and its records are pruned by the same test.

use std::{
    collections::HashMap,
    path::Path,
    sync::{LazyLock, Mutex},
};

use crate::metadata::StructMetadata;
use crate::schema_macro::file_cache::{
    FileFingerprint, get_file_content, get_file_fingerprint, with_fallback_parse,
};

/// What a registration recorded about its item's file.
pub(super) struct Recorded {
    /// The inline modules the item sat in at registration, outermost first.
    chain: Vec<String>,
    /// The file's fingerprint at registration.
    fingerprint: Option<FileFingerprint>,
    /// No textual declaration at registration: a `schema_type!`-generated DTO.
    generated: bool,
}

/// Where a stored item must still be declared to count as present.
#[derive(Clone, Copy)]
enum Scope {
    /// Anywhere in the file (the duplicate-name check).
    File,
    /// In its own inline module (a variant).
    Module,
}

/// Registration identity (`file::Ident@line:column`) → what it recorded.
///
/// Keyed by the full identity, so two same-named items of one file never share
/// a record: a `struct User` in each of two fn bodies (SM0-L2), or two sibling
/// modules (R34-SM-L1), keep their own. An item moved under the same identity
/// into another module replaces its record instead of leaving a second one
/// (SM0-L1). A moved item's record at its old position is dropped by [`record`]
/// (R27-SM0-L3), so the map is bounded by the items the session has seen, not
/// by the edits it has made.
pub(super) static FINGERPRINTS: LazyLock<Mutex<HashMap<String, Recorded>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `(file, ident)` of a `file::Ident@line:column` identity.
fn file_and_ident(identity: &str) -> Option<(&str, &str)> {
    let base = identity.rsplit_once('@').map_or(identity, |(base, _)| base);
    base.rsplit_once("::")
}

/// The line of a `file::Ident@line:column` identity (`0` when unreadable).
fn line_of(identity: &str) -> usize {
    identity
        .rsplit_once('@')
        .and_then(|(_, position)| position.split(':').next()?.parse().ok())
        .unwrap_or_default()
}

/// The inline `mod` chain around `line` in `file`, outermost first: empty at
/// the file's top level, or when the file cannot be parsed. The file is parsed
/// under the fallback (R35-SM-M1): inside rustc the compiler's spans would
/// place every brace on the call-site line, so the chain would be wrong.
/// Served by the module scope's per-content line index (SM0-L1): the file is
/// parsed once per content, not once per `#[derive(Schema)]`.
fn module_chain(file: &Path, line: usize) -> Vec<String> {
    crate::schema_macro::module_scope::inline_chain_at(file, line)
}

/// The items the inline module `chain` declares directly (child inline modules
/// are other modules and are left out); empty when `chain` is not in `items`.
fn module_own_items(items: &[syn::Item], chain: &[String]) -> Vec<syn::Item> {
    super::default_fns::inline_module_items(items.to_vec(), chain).map_or_else(Vec::new, |module| {
        module
            .into_iter()
            .filter(|item| !matches!(item, syn::Item::Mod(_)))
            .collect()
    })
}

/// How many `ident` items `items` declares directly in the inline module
/// `chain`. Child inline modules are other modules and do not count, so a
/// same-named item of a sibling module never keeps this one alive.
fn declared_in_module(items: &[syn::Item], chain: &[String], ident: &str) -> usize {
    module_own_items(items, chain)
        .iter()
        .map(|item| super::count_items_named(std::slice::from_ref(item), ident))
        .sum()
}

/// The lines of `ident` tokens in a `schema_type!` invocation's tokens (groups
/// included): the position the generated DTO's identity records.
fn ident_lines(tokens: proc_macro2::TokenStream, ident: &str, lines: &mut Vec<usize>) {
    for tree in tokens {
        match tree {
            proc_macro2::TokenTree::Ident(token) if token == ident => {
                lines.push(token.span().start().line);
            }
            proc_macro2::TokenTree::Group(group) => ident_lines(group.stream(), ident, lines),
            _ => {}
        }
    }
}

/// The lines where a `schema_type!` invocation of `items` names `ident`.
fn schema_type_lines(items: &[syn::Item], ident: &str) -> Vec<usize> {
    struct Invocations<'a> {
        ident: &'a str,
        lines: Vec<usize>,
    }
    impl<'ast> syn::visit::Visit<'ast> for Invocations<'_> {
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if mac
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "schema_type")
            {
                ident_lines(mac.tokens.clone(), self.ident, &mut self.lines);
            }
            syn::visit::visit_macro(self, mac);
        }
    }
    let mut found = Invocations {
        ident,
        lines: Vec::new(),
    };
    for item in items {
        syn::visit::Visit::visit_item(&mut found, item);
    }
    found.lines
}

/// The lines of the `schema_type!` invocations that name `ident` in the scope
/// of `items`: the whole file, or only the inline module `chain` (R41-SM0-L1:
/// a same-named invocation of a sibling module on the same line is not it).
fn invoked_lines(items: &[syn::Item], chain: &[String], ident: &str, scope: Scope) -> Vec<usize> {
    match scope {
        Scope::File => schema_type_lines(items, ident),
        Scope::Module => schema_type_lines(&module_own_items(items, chain), ident),
    }
}

/// [`invoked_lines`] of the file at `path`; `None` when it is unreadable or
/// does not parse (then nothing is pruned).
fn schema_type_lines_of(path: &Path, ident: &str, chain: &[String]) -> Option<Vec<usize>> {
    let content = get_file_content(path)?;
    with_fallback_parse(&content, |parsed| {
        invoked_lines(&parsed.items, chain, ident, Scope::Module)
    })
}

/// Whether `ident` is still declared in the given scope of `items`, or a
/// `schema_type!` invocation names it at `line`.
fn still_declared(
    items: &[syn::Item],
    chain: &[String],
    ident: &str,
    line: usize,
    generated: bool,
    scope: Scope,
) -> bool {
    let textual = match scope {
        Scope::File => super::count_items_named(items, ident),
        Scope::Module => declared_in_module(items, chain, ident),
    };
    // Only a generated DTO has no textual declaration to check: a derived item
    // that left its position is not kept by a `schema_type!` naming it there
    // (SM0-L1, round 40).
    textual > 0 || (generated && invoked_lines(items, chain, ident, scope).contains(&line))
}

/// Remember the current fingerprint of `identity`'s file and item module.
///
/// A record of the same name in the same module on another line is a moved
/// item's old position (R27-SM0-L3): dropped, but only while the file still
/// declares that name in that module (an item whose file lost it keeps its
/// record, so [`left_its_file`] can still tell it left). A generated DTO has
/// no declaration to check, so its older generated records are dropped only
/// once the invocation that named them at that position is gone (SM0-L2); a
/// deleted derive keeps its record, since it was never generated.
pub(super) fn record(identity: Option<&str>) {
    let Some(identity) = identity else {
        return;
    };
    let Some((file, ident)) = file_and_ident(identity) else {
        return;
    };
    let path = Path::new(file);
    let line = line_of(identity);
    let chain = module_chain(path, line);
    let mut records = FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = crate::schema_macro::module_scope::item_lines_in_chain(path, ident, &chain)
        .filter(|lines| !lines.is_empty());
    let generated = lines.is_none();
    match lines {
        Some(lines) => records.retain(|old, recorded| {
            file_and_ident(old) != Some((file, ident))
                || recorded.chain != chain
                || lines.contains(&line_of(old))
        }),
        None => {
            if let Some(invoked) = schema_type_lines_of(path, ident, &chain) {
                records.retain(|old, recorded| {
                    file_and_ident(old) != Some((file, ident))
                        || !recorded.generated
                        || recorded.chain != chain
                        || invoked.contains(&line_of(old))
                });
            }
        }
    }
    records.insert(
        identity.to_string(),
        Recorded {
            chain,
            fingerprint: get_file_fingerprint(path),
            generated,
        },
    );
}

/// Whether the stored `existing` duplicate-name entry's item has left its file
/// since it was registered: its name is declared nowhere in the file (SM0-L1),
/// and a generated DTO's invocation at its position is gone (SM0-L2).
pub(super) fn left_its_file(existing: &StructMetadata) -> bool {
    left(existing, Scope::File)
}

/// Whether the stored variant `existing` has left its own inline module (see
/// the module docs, R34-SM-L1).
pub(super) fn left_its_module(existing: &StructMetadata) -> bool {
    left(existing, Scope::Module)
}

fn left(existing: &StructMetadata, scope: Scope) -> bool {
    let Some(identity) = existing.source_identity.as_deref() else {
        return false;
    };
    let Some((file, ident)) = file_and_ident(identity) else {
        return false;
    };
    // The registration's own record: its module chain and fingerprint, found by
    // the stored identity that names the registration.
    let recorded = FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(identity)
        .map(|recorded| {
            (
                recorded.chain.clone(),
                recorded.fingerprint,
                recorded.generated,
            )
        });
    let path = Path::new(file);
    let line = line_of(identity);
    match recorded {
        Some((chain, fingerprint, generated)) if fingerprint != get_file_fingerprint(path) => {
            // Unparseable mid-edit but present: the item may still be in it
            // (R26-SM-L1), so only a file that is gone counts as left.
            get_file_content(path).is_none_or(|content| {
                with_fallback_parse(&content, |parsed| {
                    !still_declared(&parsed.items, &chain, ident, line, generated, scope)
                })
                .unwrap_or(false)
            })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::full("src/a.rs::User@1:7", Some(("src/a.rs", "User")))]
    #[case::no_position("src/a.rs::User", Some(("src/a.rs", "User")))]
    #[case::no_file("User@1:7", None)]
    fn identity_parts(#[case] identity: &str, #[case] expected: Option<(&str, &str)>) {
        assert_eq!(file_and_ident(identity), expected);
    }

    /// R35-SM-M1: the chain comes from the braces of the parsed file, so two
    /// same-named items of sibling inline modules get their own module.
    #[test]
    fn module_chain_follows_the_braces_of_sibling_modules() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("pages.rs");
        std::fs::write(
            &file,
            "pub mod first {
    pub struct Page {}
}
pub mod second {
    pub struct Page {}
}
pub struct Top {}
",
        )
        .unwrap();
        assert_eq!(module_chain(&file, 2), ["first"]);
        assert_eq!(module_chain(&file, 5), ["second"]);
        assert_eq!(module_chain(&file, 7), Vec::<String>::new());
        assert_eq!(
            module_chain(&dir.path().join("missing.rs"), 2),
            Vec::<String>::new()
        );
    }

    /// SM0-L1 (round 36): every `#[derive(Schema)]` of a file asks for its
    /// module chain, and the file is parsed (under the fallback) once for all
    /// of them, not once per derive.
    #[test]
    fn module_chain_parses_a_file_once_for_all_its_derives() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("pages.rs");
        std::fs::write(
            &file,
            "pub mod first {
    pub struct Page {}
}
pub struct Top {}
",
        )
        .unwrap();
        let before = crate::schema_macro::file_cache::derived_computes();
        assert_eq!(module_chain(&file, 2), ["first"]);
        assert_eq!(module_chain(&file, 2), ["first"]);
        assert_eq!(module_chain(&file, 4), Vec::<String>::new());
        assert_eq!(
            crate::schema_macro::file_cache::derived_computes() - before,
            1
        );
    }

    #[test]
    fn unrecorded_or_fileless_identity_is_never_stale() {
        let stored = |identity: Option<&str>| StructMetadata {
            source_identity: identity.map(str::to_string),
            ..StructMetadata::new("User".to_string(), String::new())
        };
        record(None);
        record(Some("User@1:7"));
        assert!(!left_its_file(&stored(None)));
        assert!(!left_its_file(&stored(Some("User@1:7"))));
        assert!(!left_its_file(&stored(Some("never/recorded.rs::User@1:7"))));
    }

    /// R27-SM0-L3: an item that edits move keeps one fingerprint entry, not one
    /// per position it was registered at.
    #[test]
    fn moved_item_keeps_one_fingerprint_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("moved.rs");
        for line in [1, 2, 9] {
            let source = format!("{}pub struct Stable {{}}\n", "\n".repeat(line - 1));
            std::fs::write(&file, source).unwrap();
            crate::schema_macro::file_cache::bump_epoch();
            record(Some(&format!("{}::Stable@{line}:11", file.display())));
        }
        let entries = FINGERPRINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|key| key.starts_with(&format!("{}::Stable", file.display())))
            .count();
        assert_eq!(entries, 1);
    }

    /// SM0-L1 (IDE-only): an item moved into an inline module at the SAME
    /// `line:column` leaves one record for its identity, under its new module
    /// chain. Two records under one identity made `left_its_file` pick one by
    /// hash-map order.
    #[test]
    fn item_moved_into_a_module_at_the_same_position_has_one_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("sl1.rs");
        let identity = format!("{}::Relocated@1:11", file.display());
        std::fs::write(&file, "pub struct Relocated {}\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        record(Some(&identity));
        std::fs::write(&file, "pub mod inner { pub struct Relocated {} }\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        record(Some(&identity));
        let records = FINGERPRINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let chains: Vec<&Vec<String>> = records
            .iter()
            .filter(|(key, _)| key.as_str() == identity)
            .map(|(_, recorded)| &recorded.chain)
            .collect();
        assert_eq!(chains, [&vec!["inner".to_string()]]);
    }

    /// SM0-L2 (IDE-only): two same-named items in two fn bodies of one module
    /// keep their own records. With one shared record for the name, deleting
    /// both items left the first one's record gone and a false duplicate-name
    /// error followed.
    #[test]
    fn same_named_items_of_two_fn_bodies_keep_their_own_records() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("bodies.rs");
        std::fs::write(
            &file,
            "fn one() {\n    struct Twin { a: i32 }\n}\nfn two() {\n    struct Twin { b: i32 }\n}\n",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let first = format!("{}::Twin@2:11", file.display());
        let second = format!("{}::Twin@5:11", file.display());
        record(Some(&first));
        record(Some(&second));
        std::fs::write(&file, "fn one() {}\nfn two() {}\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let stored = |identity: &str| StructMetadata {
            source_identity: Some(identity.to_string()),
            ..StructMetadata::new("Twin".to_string(), String::new())
        };
        assert!(left_its_file(&stored(&first)));
        assert!(left_its_file(&stored(&second)));
    }
}
