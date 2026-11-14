//! Shared per-crate storage for attribute-macro metadata.
//!
//! `#[route]` and `#[cron]` both accumulate one metadata entry per attribute
//! expansion into a process-global map keyed by
//! [`crate::schema_impl::current_crate_key`], so a long-lived rust-analyzer
//! proc-macro server (one process, many crates) never leaks crate A's entries
//! into crate B. The bookkeeping around that map — poison-tolerant locking,
//! copy-on-write bucket mutation, and replace-insert keyed by
//! `(fn_name, file_path)` — is identical for both, and lives here once.
//!
//! # Key items
//!
//! - [`CrateStorage`] - the static's type: per-crate buckets of `T`
//! - [`SourceIdentified`] - what makes two entries "the same source item"
//! - [`register`] - replace-insert one entry into the current crate's bucket
//! - [`current_crate_items`] - cheap `Arc` snapshot of the current crate's bucket

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// Type of a per-crate metadata storage static.
///
/// Keyed by [`crate::schema_impl::current_crate_key`]; each bucket is an
/// [`Arc`] so [`current_crate_items`] hands out a snapshot without deep-cloning
/// every stored entry.
pub type CrateStorage<T> = LazyLock<Mutex<HashMap<String, Arc<Vec<T>>>>>;

/// Metadata that identifies the source item (function) an entry came from.
///
/// Two entries describe the same source item when they share a function name
/// AND a source path (AND a scope, when both know theirs) — what a re-expansion
/// of the same attribute produces, which must replace rather than duplicate
/// the previous entry.
pub trait SourceIdentified {
    /// Name of the annotated function (e.g., `"get_user"`).
    fn fn_name(&self) -> &str;
    /// Source file path, when the compiler exposed one.
    fn file_path(&self) -> Option<&str>;
    /// Where inside the file the item lives (e.g. its inline `mod` path), when
    /// known.  Two known scopes must match; an unknown scope matches any, so an
    /// entry whose scope could not be located still replaces rather than
    /// duplicates.
    fn scope(&self) -> Option<&str> {
        None
    }
}

/// Whether two entries came from the same function in the same file (and the
/// same scope inside it), treating `\` and `/` as equivalent separators.
fn same_source<T: SourceIdentified>(left: &T, right: &T) -> bool {
    left.fn_name() == right.fn_name()
        && crate::file_utils::paths_equal_normalized(left.file_path(), right.file_path())
        && match (left.scope(), right.scope()) {
            (Some(left), Some(right)) => left == right,
            _ => true,
        }
}

/// Replace-insert one metadata entry in the current crate's bucket.
///
/// A re-expansion of the same source item (same function name, same normalized
/// file path) overwrites the previous entry; anything else is appended.
pub fn register<T: Clone + SourceIdentified>(storage: &CrateStorage<T>, info: T) {
    let mut guard = storage.lock().unwrap_or_else(PoisonError::into_inner);
    let bucket = Arc::make_mut(
        guard
            .entry(crate::schema_impl::current_crate_key())
            .or_insert_with(|| Arc::new(Vec::new())),
    );
    if let Some(existing) = bucket
        .iter_mut()
        .find(|existing| same_source(&**existing, &info))
    {
        *existing = info;
    } else {
        bucket.push(info);
    }
}

/// Snapshot of the current crate's bucket — a cheap `Arc` clone, so consumers
/// never deep-clone every stored entry.
#[must_use]
pub fn current_crate_items<T>(storage: &CrateStorage<T>) -> Arc<Vec<T>> {
    storage
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&crate::schema_impl::current_crate_key())
        .cloned()
        .unwrap_or_else(|| Arc::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestEntry {
        fn_name: String,
        file_path: Option<String>,
        payload: &'static str,
        scope: Option<&'static str>,
    }

    impl SourceIdentified for TestEntry {
        fn fn_name(&self) -> &str {
            &self.fn_name
        }
        fn file_path(&self) -> Option<&str> {
            self.file_path.as_deref()
        }
        fn scope(&self) -> Option<&str> {
            self.scope
        }
    }

    static TEST_STORAGE: CrateStorage<TestEntry> = LazyLock::new(|| Mutex::new(HashMap::new()));

    fn entry(fn_name: &str, file_path: &str, payload: &'static str) -> TestEntry {
        TestEntry {
            fn_name: fn_name.to_string(),
            file_path: Some(file_path.to_string()),
            payload,
            scope: None,
        }
    }

    fn scoped(scope: Option<&'static str>, payload: &'static str) -> TestEntry {
        TestEntry {
            scope,
            ..entry("__scoped", "C:/vespera/jobs.rs", payload)
        }
    }

    /// Known scopes must match; an unknown scope matches any.
    #[test]
    fn register_keys_by_scope_when_both_know_it() {
        register(&TEST_STORAGE, scoped(Some("a"), "a1"));
        register(&TEST_STORAGE, scoped(Some("b"), "b1"));
        assert_eq!(payloads_for("__scoped"), vec!["a1", "b1"]);
        register(&TEST_STORAGE, scoped(Some("b"), "b2"));
        assert_eq!(payloads_for("__scoped"), vec!["a1", "b2"]);
        register(&TEST_STORAGE, scoped(None, "unknown"));
        assert_eq!(payloads_for("__scoped"), vec!["unknown", "b2"]);
    }

    fn payloads_for(fn_name: &str) -> Vec<&'static str> {
        current_crate_items(&TEST_STORAGE)
            .iter()
            .filter(|found| found.fn_name == fn_name)
            .map(|found| found.payload)
            .collect()
    }

    #[test]
    fn register_replaces_same_fn_with_separator_equivalent_path() {
        // `\` vs `/` must compare equal, so the second registration REPLACES.
        register(
            &TEST_STORAGE,
            entry("__replace_me", r"C:\vespera\routes\a.rs", "before"),
        );
        register(
            &TEST_STORAGE,
            entry("__replace_me", "C:/vespera/routes/a.rs", "after"),
        );

        assert_eq!(payloads_for("__replace_me"), vec!["after"]);
    }

    #[test]
    fn register_pushes_different_fn_name_at_same_path() {
        register(
            &TEST_STORAGE,
            entry("__push_first", "C:/vespera/routes/b.rs", "first"),
        );
        register(
            &TEST_STORAGE,
            entry("__push_second", "C:/vespera/routes/b.rs", "second"),
        );

        assert_eq!(payloads_for("__push_first"), vec!["first"]);
        assert_eq!(payloads_for("__push_second"), vec!["second"]);
    }

    #[test]
    fn current_crate_items_is_empty_for_unused_storage() {
        static EMPTY_STORAGE: CrateStorage<TestEntry> =
            LazyLock::new(|| Mutex::new(HashMap::new()));
        assert!(current_crate_items(&EMPTY_STORAGE).is_empty());
    }
}
