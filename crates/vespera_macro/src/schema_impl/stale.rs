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
//! item of that name (or is gone). An unchanged file — every file of a
//! `cargo build` — keeps the strict duplicate-name error.

use std::{
    collections::HashMap,
    path::Path,
    sync::{LazyLock, Mutex},
};

use crate::metadata::StructMetadata;
use crate::schema_macro::file_cache::{FileFingerprint, get_file_fingerprint, get_parsed_file};

/// Source identity → its file's fingerprint when it was registered.
static FINGERPRINTS: LazyLock<Mutex<HashMap<String, Option<FileFingerprint>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `(file, ident)` of a `file::Ident@line:column` identity.
fn file_and_ident(identity: &str) -> Option<(&str, &str)> {
    let base = identity.rsplit_once('@').map_or(identity, |(base, _)| base);
    base.rsplit_once("::")
}

/// Remember the current fingerprint of `identity`'s file.
pub(super) fn record(identity: Option<&str>) {
    let Some((identity, (file, _))) = identity.and_then(|id| Some((id, file_and_ident(id)?)))
    else {
        return;
    };
    let fingerprint = get_file_fingerprint(Path::new(file));
    FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(identity.to_string(), fingerprint);
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
    let recorded = FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(identity)
        .copied();
    let path = Path::new(file);
    match recorded {
        Some(fingerprint) if fingerprint != get_file_fingerprint(path) => get_parsed_file(path)
            .is_none_or(|parsed| super::count_items_named(&parsed.items, ident) == 0),
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
}
