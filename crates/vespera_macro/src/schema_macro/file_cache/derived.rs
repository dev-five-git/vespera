//! Plain data derived from a file's content, cached across macro invocations.
//!
//! A `syn::File` cannot outlive the invocation that parsed it (it holds
//! compiler-bridge handles), so callers that need only *plain data* from a
//! parse — names, line numbers, module paths, cfg strings — re-parsed the
//! whole file on every call.  [`derived`] keeps such a result next to the
//! content it was computed from.
//!
//! **Validity is exactly the content cache's.**  An entry holds the
//! `Arc<String>` of the content it was derived from and is reused only while
//! [`get_file_content`](super::get_file_content) still hands out that very
//! allocation (`Arc::ptr_eq`): the content cache replaces the `Arc` as soon
//! as the file's mtime+len fingerprint changes (revalidated once per epoch,
//! so an edit under rust-analyzer's long-lived server is seen by the next
//! invocation), and content it cannot cache (no fingerprint) is a fresh
//! allocation per call — never reused.  Holding the old `Arc` keeps its
//! address from being recycled while the entry lives.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// `(file, derived type)` → (content derived from, value).
type Entries = HashMap<(PathBuf, TypeId), (Arc<String>, Rc<dyn Any>)>;

thread_local! {
    static DERIVED: RefCell<Entries> = RefCell::new(HashMap::new());
}

#[cfg(test)]
thread_local! {
    static COMPUTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many times [`derived`] ran a `compute` on this thread.
#[cfg(test)]
pub fn derived_computes() -> usize {
    COMPUTES.with(std::cell::Cell::get)
}

/// The `T` derived from `path`'s content by `compute`, computed at most once
/// per content version (see the module docs).  `None` when the file cannot
/// be read.
///
/// One `T` per file: give each kind of derived data its own type.  `compute`
/// runs with no cache borrowed, so it may read other files through the file
/// cache.
pub fn derived<T: 'static>(path: &Path, compute: impl FnOnce(&str) -> T) -> Option<Rc<T>> {
    let content = super::get_file_content(path)?;
    let key = (path.to_path_buf(), TypeId::of::<T>());
    let hit = DERIVED.with(|cache| {
        cache
            .borrow()
            .get(&key)
            .filter(|(source, _)| Arc::ptr_eq(source, &content))
            .map(|(_, value)| Rc::clone(value))
    });
    if let Some(value) = hit.and_then(|value| value.downcast::<T>().ok()) {
        return Some(value);
    }
    #[cfg(test)]
    COMPUTES.with(|count| count.set(count.get() + 1));
    let value = Rc::new(compute(&content));
    let erased: Rc<dyn Any> = Rc::clone(&value) as Rc<dyn Any>;
    DERIVED.with(|cache| cache.borrow_mut().insert(key, (content, erased)));
    Some(value)
}

/// [`Path::canonicalize`], memoised for the current macro invocation (the
/// test-scope and cron module-tree walks canonicalize the same files once
/// per schema / route / module).
pub fn canonicalize_cached(path: &Path) -> Option<PathBuf> {
    type Memo = (u64, HashMap<PathBuf, Option<PathBuf>>);
    thread_local! {
        static MEMO: RefCell<Memo> = RefCell::new((u64::MAX, HashMap::new()));
    }
    let epoch = super::current_epoch();
    let hit = MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.0 != epoch {
            *memo = (epoch, HashMap::new());
        }
        memo.1.get(path).cloned()
    });
    if let Some(canonical) = hit {
        return canonical;
    }
    let canonical = path.canonicalize().ok();
    MEMO.with(|memo| {
        memo.borrow_mut()
            .1
            .insert(path.to_path_buf(), canonical.clone())
    });
    canonical
}

/// Restores proc-macro2's normal mode even if parsing panics (declared
/// outside the generic [`with_fallback_parse`]: one non-generic `Drop`
/// impl).
struct Unforce;

impl Drop for Unforce {
    fn drop(&mut self) {
        proc_macro2::fallback::unforce();
    }
}

/// Parse `content` with proc-macro2's fallback implementation forced, so
/// every span carries its real line / column (tokens parsed from a string
/// through the compiler bridge all carry the call-site span), and hand the
/// file to `read`.  Nothing parsed here may leave `read`: only plain data
/// does, so no fallback token is ever mixed with compiler tokens.
pub fn with_fallback_parse<T>(content: &str, read: impl FnOnce(&syn::File) -> T) -> Option<T> {
    proc_macro2::fallback::force();
    let _unforce = Unforce;
    syn::parse_file(content).ok().map(|file| read(&file))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Len(usize);

    #[test]
    fn computes_once_per_content_version() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("a.rs");
        std::fs::write(&path, "fn a() {}").unwrap();
        super::super::bump_epoch();
        let calls = Cell::new(0);
        let compute = |content: &str| {
            calls.set(calls.get() + 1);
            Len(content.len())
        };
        assert_eq!(derived(&path, compute).unwrap().0, 9);
        super::super::bump_epoch();
        assert_eq!(derived(&path, compute).unwrap().0, 9);
        assert_eq!(calls.get(), 1, "unchanged content is served from the cache");

        // A size-changing edit is seen by the next epoch.
        std::fs::write(&path, "fn ab() {}").unwrap();
        super::super::bump_epoch();
        assert_eq!(derived(&path, compute).unwrap().0, 10);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn distinct_types_and_missing_files() {
        struct Other(usize);
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("a.rs");
        std::fs::write(&path, "x").unwrap();
        super::super::bump_epoch();
        assert_eq!(derived(&path, |c| Len(c.len())).unwrap().0, 1);
        assert_eq!(derived(&path, |_| Other(7)).unwrap().0, 7);
        assert!(derived(&temp.path().join("missing.rs"), |c| Len(c.len())).is_none());
    }

    #[test]
    fn fallback_parse_reports_lines_and_rejects_bad_input() {
        assert_eq!(
            with_fallback_parse("\n\nfn a() {}", |file| match &file.items[0] {
                syn::Item::Fn(f) => f.sig.ident.span().start().line,
                _ => 0,
            }),
            Some(3)
        );
        assert_eq!(with_fallback_parse("fn {", |_| ()), None);
    }
}
