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
    fn scope(&self) -> Option<&str>;
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

/// The entries whose source file still declares their function.
///
/// [`register`] replaces an entry only when the SAME function is expanded
/// again, so in a long-lived rust-analyzer proc-macro server a renamed or
/// deleted handler / job leaves its old entry behind — and `vespera!` would
/// mount a function that no longer exists.  Inside that server, entries are
/// checked against the current file text (the fingerprint-validated
/// `file_cache`, so a file is read once per change); one without a file path
/// or whose file cannot be read is kept.  Under rustc the storage only ever
/// holds the current compilation, so nothing is filtered: a handler / job a
/// `macro_rules!` macro generates (`h!(list)`) has no literal `fn list` in
/// its file and must not be dropped.  Borrows when nothing is stale.
pub fn retain_declared<T: Clone + SourceIdentified>(items: &[T]) -> std::borrow::Cow<'_, [T]> {
    retain_declared_if(items, in_long_lived_server())
}

/// Whether this process is rust-analyzer's long-lived proc-macro server,
/// whose storage keeps entries of edited-away items.
pub(crate) fn in_long_lived_server() -> bool {
    static IN_IDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN_IDE.get_or_init(|| is_rust_analyzer(std::env::current_exe().ok().as_deref()))
        || test_ide::forced()
}

/// Test-only switch emulating the IDE for [`retain_declared`].
#[cfg(test)]
pub(crate) mod test_ide {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn forced() -> bool {
        FORCED.with(Cell::get)
    }

    /// Emulates rust-analyzer on this thread until the guard drops.
    pub(crate) fn force() -> impl Drop {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                FORCED.with(|forced| forced.set(false));
            }
        }
        FORCED.with(|forced| forced.set(true));
        Restore
    }
}

#[cfg(not(test))]
mod test_ide {
    pub(super) const fn forced() -> bool {
        false
    }
}

/// Whether `exe` is rust-analyzer's (long-lived) proc-macro server.
pub(crate) fn is_rust_analyzer(exe: Option<&std::path::Path>) -> bool {
    exe.and_then(std::path::Path::file_name)
        .is_some_and(|name| name.to_string_lossy().contains("rust-analyzer"))
}

/// [`retain_declared`], filtering only when `filter` (inside the IDE).
fn retain_declared_if<T: Clone + SourceIdentified>(
    items: &[T],
    filter: bool,
) -> std::borrow::Cow<'_, [T]> {
    let declared = |item: &T| {
        item.file_path().is_none_or(|file| {
            crate::schema_macro::file_cache::get_file_content(std::path::Path::new(file))
                .is_none_or(|content| declares_fn(&content, item.fn_name()))
        })
    };
    if !filter || items.iter().all(declared) {
        std::borrow::Cow::Borrowed(items)
    } else {
        std::borrow::Cow::Owned(
            items
                .iter()
                .filter(|item| declared(item))
                .cloned()
                .collect(),
        )
    }
}

/// Whether `content` contains `fn <name>` (the name as a whole identifier,
/// separated from `fn` by whitespace).  Textual on purpose — a cheap check
/// that never parses; a mention in a comment only keeps an entry alive.
fn declares_fn(content: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    content.match_indices(name).any(|(at, _)| {
        let ends_ident = content[at + name.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident(c));
        let before = &content[..at];
        let keyword = before.trim_end();
        ends_ident
            && keyword.len() < before.len()
            && keyword
                .strip_suffix("fn")
                .is_some_and(|rest| rest.chars().next_back().is_none_or(|c| !is_ident(c)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::plain("pub async fn get_user() {}", "get_user", true)]
    #[case::newline("fn\n  get_user()", "get_user", true)]
    #[case::raw("pub fn r#type() {}", "r#type", true)]
    #[case::at_start_of_file("fn a() {}", "a", true)]
    #[case::renamed("pub async fn get_user_v2() {}", "get_user", false)]
    #[case::prefix_ident("pub async fn my_get_user() {}", "get_user", false)]
    #[case::not_a_declaration("let get_user = 1; get_user();", "get_user", false)]
    #[case::fn_suffix_of_ident("pub async fnx get_user() {}", "get_user", false)]
    #[case::no_space("fn(get_user)", "get_user", false)]
    #[case::absent("", "get_user", false)]
    fn declares_fn_matches_whole_declarations(
        #[case] content: &str,
        #[case] name: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(declares_fn(content, name), expected);
    }

    /// A renamed handler's old entry is dropped; entries without a readable
    /// file are kept, and nothing is cloned when nothing is stale.
    #[test]
    fn retain_declared_drops_entries_whose_function_is_gone() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("users.rs");
        std::fs::write(&file, "pub async fn renamed() {}").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let path = file.display().to_string();
        let items = vec![
            entry("old_name", &path, "stale"),
            entry("renamed", &path, "live"),
            entry("anything", "missing/file.rs", "unreadable"),
            TestEntry {
                file_path: None,
                ..entry("pathless", "", "pathless")
            },
        ];
        let kept = retain_declared_if(&items, true);
        assert!(matches!(kept, std::borrow::Cow::Owned(_)));
        let payloads: Vec<_> = kept.iter().map(|item| item.payload).collect();
        assert_eq!(payloads, ["live", "unreadable", "pathless"]);
        let live = retain_declared_if(&items[1..], true);
        assert!(matches!(live, std::borrow::Cow::Borrowed(_)));
        // Outside the IDE (rustc, this test binary) nothing is filtered: a
        // `macro_rules!`-generated function has no literal `fn name`.
        assert!(matches!(retain_declared(&items), std::borrow::Cow::Borrowed(all) if all.len() == 4));
    }

    #[rstest::rstest]
    #[case::server(Some("rust-analyzer-proc-macro-srv"), true)]
    #[case::exe(Some("rust-analyzer.exe"), true)]
    #[case::rustc(Some("rustc"), false)]
    #[case::unknown(None, false)]
    fn is_rust_analyzer_matches_the_server_exe(#[case] exe: Option<&str>, #[case] expected: bool) {
        assert_eq!(is_rust_analyzer(exe.map(std::path::Path::new)), expected);
    }

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
