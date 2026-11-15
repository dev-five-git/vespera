//! Results computed from several files, reused while none of them changed.
//!
//! [`derived`](super::derived) covers data read off ONE file.  A module
//! scope or a type-alias resolution walks a module tree: which files it
//! opened (and which candidates it probed and found missing) is only known
//! while it runs.  [`tracked`] records every fingerprint and directory
//! listing the file cache serves during `compute`; [`Deps::still_valid`]
//! re-checks them all through the same per-epoch fingerprint cache — so a
//! dependency edited, created or deleted under rust-analyzer's long-lived
//! server invalidates the result in the next invocation, exactly like the
//! content cache itself.
//!
//! Nested tracking composes: a finished inner computation adds its
//! dependencies to the enclosing one, and validating a cached inner result
//! reads its dependencies through the cache again, which records them in the
//! enclosing computation too.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::FileFingerprint;

/// One input a tracked computation read.
#[derive(Clone, PartialEq, Eq)]
enum Dep {
    /// A file's fingerprint (`None`: the file did not exist / was unreadable).
    File(PathBuf, Option<FileFingerprint>),
    /// A directory's `.rs` listing fingerprint (see `ensure_file_list`).
    Dir(PathBuf, u64),
}

impl Dep {
    fn path(&self) -> (u8, &Path) {
        match self {
            Self::File(path, _) => (0, path),
            Self::Dir(path, _) => (1, path),
        }
    }
}

/// The inputs of one [`tracked`] computation.
#[derive(Clone, Default)]
pub struct Deps(Vec<Dep>);

impl Deps {
    /// Whether every recorded input still reads the same.
    pub fn still_valid(&self) -> bool {
        self.0.iter().all(|dep| match dep {
            Dep::File(path, fingerprint) => super::get_file_fingerprint(path) == *fingerprint,
            Dep::Dir(path, fingerprint) => super::dir_fingerprint(path) == *fingerprint,
        })
    }
}

thread_local! {
    /// One frame per active [`tracked`] computation, innermost last.
    static FRAMES: RefCell<Vec<Vec<Dep>>> = const { RefCell::new(Vec::new()) };
}

fn record(dep: Dep) {
    FRAMES.with(|frames| {
        if let Some(frame) = frames.borrow_mut().last_mut() {
            frame.push(dep);
        }
    });
}

/// Record a file fingerprint the cache served (no-op outside [`tracked`]).
pub(super) fn record_file(path: &Path, fingerprint: Option<FileFingerprint>) {
    record(Dep::File(path.to_path_buf(), fingerprint));
}

/// Record a directory listing the cache served (no-op outside [`tracked`]).
pub(super) fn record_dir(path: &Path, fingerprint: u64) {
    record(Dep::Dir(path.to_path_buf(), fingerprint));
}

/// Run `compute`, returning its result with every file-cache input it read.
pub fn tracked<T>(compute: impl FnOnce() -> T) -> (T, Deps) {
    /// Pops this computation's frame even if `compute` panics.
    struct Frame;
    impl Drop for Frame {
        fn drop(&mut self) {
            FRAMES.with(|frames| frames.borrow_mut().pop());
        }
    }
    FRAMES.with(|frames| frames.borrow_mut().push(Vec::new()));
    let frame = Frame;
    let value = compute();
    // Popped here instead of by the guard (which only covers a panic).
    std::mem::forget(frame);
    let mut deps = FRAMES
        .with(|frames| frames.borrow_mut().pop())
        .unwrap_or_default();
    deps.sort_by(|a, b| a.path().cmp(&b.path()));
    deps.dedup_by(|a, b| a.path() == b.path());
    // The enclosing computation depends on these inputs too.
    FRAMES.with(|frames| {
        if let Some(parent) = frames.borrow_mut().last_mut() {
            parent.extend(deps.iter().cloned());
        }
    });
    (value, Deps(deps))
}

/// A cross-invocation memo entry: the value, its inputs and the epoch they
/// were last confirmed in (checked once per epoch).
pub struct Tracked<T> {
    value: T,
    deps: Deps,
    validated_epoch: u64,
}

impl<T: Clone> Tracked<T> {
    /// Run `compute` tracked.
    pub fn compute(compute: impl FnOnce() -> T) -> Self {
        let (value, deps) = tracked(compute);
        Self {
            value,
            deps,
            validated_epoch: super::current_epoch(),
        }
    }

    /// The value as computed (unvalidated).
    pub const fn peek(&self) -> &T {
        &self.value
    }

    /// The value while its inputs are unchanged (confirmed once per epoch).
    /// Confirming reads the inputs through the file cache, so an enclosing
    /// [`tracked`] computation records them.
    pub fn get(&mut self) -> Option<T> {
        let epoch = super::current_epoch();
        if self.validated_epoch == epoch {
            // Same epoch: report the inputs to an enclosing computation.
            for dep in &self.deps.0 {
                record(dep.clone());
            }
        } else {
            if !self.deps.still_valid() {
                return None;
            }
            self.validated_epoch = epoch;
        }
        Some(self.value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_reads_and_detects_edits_creations_and_listings() {
        let temp = tempfile::TempDir::new().unwrap();
        let present = temp.path().join("a.rs");
        let absent = temp.path().join("b.rs");
        std::fs::write(&present, "x").unwrap();
        super::super::bump_epoch();
        let mut memo = Tracked::compute(|| {
            let found = super::super::get_file_content(&present).is_some();
            let missing = super::super::get_file_content(&absent).is_none();
            let listed = super::super::list_rs_files(temp.path()).len();
            (found, missing, listed)
        });
        assert_eq!(memo.get(), Some((true, true, 1)));
        super::super::bump_epoch();
        assert_eq!(memo.get(), Some((true, true, 1)), "unchanged inputs");

        // An outer computation inherits a cached inner result's inputs.
        let (_, outer) = tracked(|| memo.get());
        assert!(outer.still_valid());
        assert!(!outer.0.is_empty());

        // Creating a probed-but-missing file invalidates.
        std::fs::write(&absent, "y").unwrap();
        super::super::bump_epoch();
        assert_eq!(memo.get(), None);
        assert!(!outer.still_valid());

        // Editing a read file invalidates.
        let mut memo =
            Tracked::compute(|| super::super::get_file_content(&present).map(|c| c.len()));
        std::fs::write(&present, "xyz").unwrap();
        super::super::bump_epoch();
        assert_eq!(memo.get(), None);

        // A listing alone: a new file in the directory invalidates.
        let mut memo = Tracked::compute(|| super::super::list_rs_files(temp.path()).len());
        super::super::bump_epoch();
        assert_eq!(memo.get(), Some(2));
        std::fs::write(temp.path().join("c.rs"), "z").unwrap();
        super::super::bump_epoch();
        assert_eq!(memo.get(), None);
    }

    #[test]
    fn nested_tracking_propagates_and_survives_panics() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("a.rs");
        std::fs::write(&path, "x").unwrap();
        super::super::bump_epoch();
        let ((), outer) = tracked(|| {
            let (_, inner) = tracked(|| super::super::get_file_content(&path));
            assert_eq!(inner.0.len(), 1);
        });
        assert_eq!(outer.0.len(), 1);
        let caught = std::panic::catch_unwind(|| tracked::<()>(|| panic!("boom")));
        assert!(caught.is_err());
        FRAMES.with(|frames| assert!(frames.borrow().is_empty()));
        // Outside any computation nothing is recorded.
        record_file(&path, None);
        FRAMES.with(|frames| assert!(frames.borrow().is_empty()));
    }
}
