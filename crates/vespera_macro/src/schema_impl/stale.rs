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
//! Staleness is decided per module (R34-SM-L1): a same-named item of a sibling
//! inline `mod` in the same file does not keep a deleted item alive.

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
struct Recorded {
    /// The identity (`file::Ident@line:column`) the item was registered under.
    identity: String,
    /// The inline modules the item sat in at registration, outermost first.
    chain: Vec<String>,
    /// The file's fingerprint at registration.
    fingerprint: Option<FileFingerprint>,
}

/// Item key (`file::module::Ident`) → what its registration recorded.
///
/// Keyed by the item and its module, not by its `line:column`: an edit that
/// moves an item re-registers it under its new position, and a position-keyed
/// map would keep the old entry for ever (R27-SM0-L3). Two same-named items of
/// sibling modules of one file get two keys (R34-SM-L1). One entry per item
/// bounds the map by the items the session has seen, not by the edits it has
/// made.
static FINGERPRINTS: LazyLock<Mutex<HashMap<String, Recorded>>> =
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
fn module_chain(file: &Path, line: usize) -> Vec<String> {
    get_file_content(file)
        .and_then(|content| {
            with_fallback_parse(&content, |parsed| enclosing_modules(&parsed.items, line))
        })
        .unwrap_or_default()
}

/// The inline modules whose braces span `line` (as the openapi generator's
/// `nested` finds them).
fn enclosing_modules(items: &[syn::Item], line: usize) -> Vec<String> {
    items
        .iter()
        .find_map(|item| match item {
            syn::Item::Mod(syn::ItemMod {
                ident,
                content: Some((brace, inner)),
                ..
            }) if (brace.span.open().start().line..=brace.span.close().end().line)
                .contains(&line) =>
            {
                let mut chain = vec![syn::ext::IdentExt::unraw(ident).to_string()];
                chain.extend(enclosing_modules(inner, line));
                Some(chain)
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// The [`FINGERPRINTS`] key of the item `ident` in module `chain` of `file`.
fn item_key(file: &str, chain: &[String], ident: &str) -> String {
    chain
        .iter()
        .map(String::as_str)
        .chain([ident])
        .fold(file.to_string(), |key, segment| format!("{key}::{segment}"))
}

/// How many `ident` items `items` declares directly in the inline module
/// `chain`. Child inline modules are other modules and do not count, so a
/// same-named item of a sibling module never keeps this one alive.
fn declared_in_module(items: &[syn::Item], chain: &[String], ident: &str) -> usize {
    super::default_fns::inline_module_items(items.to_vec(), chain).map_or(0, |module| {
        module
            .iter()
            .filter(|item| !matches!(item, syn::Item::Mod(_)))
            .map(|item| super::count_items_named(std::slice::from_ref(item), ident))
            .sum()
    })
}

/// Remember the current fingerprint of `identity`'s file and item module.
pub(super) fn record(identity: Option<&str>) {
    let Some(identity) = identity else {
        return;
    };
    let Some((file, ident)) = file_and_ident(identity) else {
        return;
    };
    let path = Path::new(file);
    let chain = module_chain(path, line_of(identity));
    let key = item_key(file, &chain, ident);
    let recorded = Recorded {
        identity: identity.to_string(),
        chain,
        fingerprint: get_file_fingerprint(path),
    };
    FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, recorded);
}

/// Whether the stored `existing` entry's item has left its file since it was
/// registered (see the module docs).
pub(super) fn left_its_file(existing: &StructMetadata) -> bool {
    let Some(identity) = existing.source_identity.as_deref() else {
        return false;
    };
    let Some((file, ident)) = file_and_ident(identity) else {
        return false;
    };
    // The registration's own record: its module chain and fingerprint. The
    // stored identity names the registration, so the record is found by it.
    let recorded = FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .find(|recorded| recorded.identity == identity)
        .map(|recorded| (recorded.chain.clone(), recorded.fingerprint));
    let path = Path::new(file);
    match recorded {
        Some((chain, fingerprint)) if fingerprint != get_file_fingerprint(path) => {
            // Unparseable mid-edit but present: the item may still be in it
            // (R26-SM-L1), so only a file that is gone counts as left.
            get_file_content(path).is_none_or(|content| {
                with_fallback_parse(&content, |parsed| {
                    declared_in_module(&parsed.items, &chain, ident) == 0
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
        record(Some("growth/moved.rs::Stable@1:1"));
        record(Some("growth/moved.rs::Stable@2:1"));
        record(Some("growth/moved.rs::Stable@9:4"));
        let entries = FINGERPRINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|key| key.starts_with("growth/moved.rs::Stable"))
            .count();
        assert_eq!(entries, 1);
    }
}
