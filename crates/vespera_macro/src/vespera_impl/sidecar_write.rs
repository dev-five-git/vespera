//! Crash- and race-safe writes of the files `include_str!` reads.
//!
//! A package's lib, bin and test targets expand the same `vespera!` /
//! `export_app!` in parallel `rustc` processes.  Their specs can differ (e.g.
//! `#[cfg(test)]` routes), so a shared file rewritten in place could be read
//! half-written or with the other target's content.  Three measures prevent
//! it:
//!
//! * **atomic replace** — content goes to a unique temp file first and is
//!   renamed over the target, so a reader sees the old or the new file, never
//!   a torn one;
//! * **content addressing** — `include_str!` targets carry the content hash in
//!   their name, so each expansion embeds exactly the bytes it generated.
//!   Superseded files are pruned once they have been superseded for long
//!   enough that no in-flight compilation can still be about to read them
//!   (the grace period runs from the moment the selection moved to the file
//!   that superseded them, kept in a `<stem>.selected` marker, not from their
//!   own creation or from a reselection of an unchanged file);
//! * **per-variant names** — the library, a binary, the `--test` harness and
//!   rust-analyzer get their own file stems (`path_utils::target_variant`),
//!   so pruning only ever removes an older file of the SAME build variant.
//!   Sharing one stem, `cargo test` pruned the library's still-referenced
//!   file once it was ten minutes old (it is never rewritten while its
//!   content is unchanged), and the next `cargo build` failed to read it.
//!
//! A kept file's mtime is deliberately never refreshed: it is an
//! `include_str!` input in cargo's dep-info, and an mtime newer than the
//! build start makes cargo rebuild the crate on every invocation.

use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

/// How long a superseded content-addressed file is kept for a parallel
/// compilation that may still `include_str!` it.
const PRUNE_AFTER: Duration = Duration::from_secs(10 * 60);

/// Replace `path` with `content` atomically, skipping the write when it
/// already holds exactly that.
pub(super) fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if super::openapi_io::content_unchanged(path, content) {
        return Ok(());
    }
    let file_name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let temp = path.with_file_name(format!(
        ".{file_name}.{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(err) = std::fs::write(&temp, content) {
        // A partial write (disk full, permissions) must not leave the temp file.
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        // A concurrent writer installed the same bytes first (Windows refuses
        // to replace a file another process holds open).
        if !super::openapi_io::content_unchanged(path, content) {
            return Err(err);
        }
    }
    Ok(())
}

/// `<dir>/<stem>-<hash>.json`, the content-addressed name for `content`.
pub(super) fn content_addressed(dir: &Path, stem: &str, hash: u64) -> PathBuf {
    dir.join(format!("{stem}-{hash:016x}.json"))
}

/// Remove `<stem>-<16 hex>.json` siblings of `keep` older than
/// [`PRUNE_AFTER`].  Best-effort: failures only leave a stale file behind.
pub(super) fn prune_superseded(keep: &Path, stem: &str) {
    prune_superseded_older_than(keep, stem, PRUNE_AFTER);
}

/// The `<stem>.selected` marker: it names the file `stem` last selected, and
/// its mtime is when the selection last moved there.
fn record_selection(dir: &Path, stem: &str, keep: &Path) -> PathBuf {
    let marker = dir.join(format!("{stem}.selected"));
    let name = keep
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    // Rewritten only when the selection moves: a kept file reselected without
    // a rewrite keeps the time it was superseded from, not the reselection.
    if !std::fs::read_to_string(&marker).is_ok_and(|current| current == name) {
        let _ = std::fs::write(&marker, name);
    }
    marker
}

fn prune_superseded_older_than(keep: &Path, stem: &str, age: Duration) {
    let Some(dir) = keep.parent() else {
        return;
    };
    let marker = record_selection(dir, stem, keep);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{stem}-");
    let now = SystemTime::now();
    // A superseded file was superseded when the selection last moved to the
    // kept file (the marker's mtime). The kept file's own mtime is not used:
    // it is not refreshed when the file is selected again.
    let selected_at = std::fs::metadata(&marker)
        .and_then(|meta| meta.modified())
        .ok();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_sibling = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".json"))
            .is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| {
                let superseded_at = selected_at.map_or(modified, |selected| selected.max(modified));
                now.duration_since(superseded_at).unwrap_or_default() >= age
            });
        if is_sibling && stale && path != keep {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn atomic_write_replaces_and_skips_identical_content() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("spec.json");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        write_atomic(&path, "two").unwrap();
        // No temp file is left behind.
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_reports_an_unwritable_target() {
        let temp = TempDir::new().unwrap();
        // The parent "directory" is a file: the temp write fails.
        let blocker = temp.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        assert!(write_atomic(&blocker.join("spec.json"), "x").is_err());
        // The target is a directory: the rename fails and is reported.
        let dir_target = temp.path().join("taken");
        fs::create_dir(&dir_target).unwrap();
        fs::write(dir_target.join("inner"), "").unwrap();
        assert!(write_atomic(&dir_target, "x").is_err());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn prune_removes_only_old_superseded_siblings() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let keep = content_addressed(dir, "vespera_spec-app", 1);
        let old = content_addressed(dir, "vespera_spec-app", 2);
        let other_stem = content_addressed(dir, "vespera_spec-app-x", 3);
        for path in [&keep, &old, &other_stem] {
            fs::write(path, "{}").unwrap();
        }
        fs::write(dir.join("vespera_spec-app-short.json"), "{}").unwrap();

        // Fresh files survive the default grace period.
        prune_superseded(&keep, "vespera_spec-app");
        assert!(old.exists());

        prune_superseded_older_than(&keep, "vespera_spec-app", Duration::ZERO);
        assert!(keep.exists());
        assert!(!old.exists());
        assert!(other_stem.exists());
        assert!(dir.join("vespera_spec-app-short.json").exists());

        // A missing directory / root path is a no-op.
        prune_superseded(&dir.join("missing").join("x.json"), "x");
        prune_superseded(Path::new("/"), "x");
    }

    fn set_age(path: &Path, ago: Duration) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - ago)
            .unwrap();
    }

    /// The grace period of a superseded file runs from the selection that
    /// superseded it, not from its own creation: a file written an hour ago,
    /// superseded just now, is kept.
    #[test]
    fn prune_grace_runs_from_supersession_not_creation() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let keep = content_addressed(dir, "vespera_spec-app", 1);
        let old = content_addressed(dir, "vespera_spec-app", 2);
        fs::write(&old, "{}").unwrap();
        set_age(&old, Duration::from_secs(60 * 60));
        fs::write(&keep, "{}").unwrap();

        prune_superseded_older_than(&keep, "vespera_spec-app", Duration::from_secs(10 * 60));
        assert!(old.exists());

        // Once it has been superseded for the whole grace period, it goes.
        set_age(
            &dir.join("vespera_spec-app.selected"),
            Duration::from_secs(60 * 60),
        );
        prune_superseded_older_than(&keep, "vespera_spec-app", Duration::from_secs(10 * 60));
        assert!(!old.exists());
        assert!(keep.exists());
    }

    /// Reselecting a kept file without rewriting it moves the supersession
    /// time of the file it replaced: `b` superseded by `a`'s reselection just
    /// now survives its grace period, though `b` and `a` were both written
    /// long ago.
    #[test]
    fn reselecting_an_unchanged_file_restarts_the_grace_of_the_one_it_replaced() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let stem = "vespera_spec-app";
        let a = content_addressed(dir, stem, 1);
        let b = content_addressed(dir, stem, 2);
        let marker = dir.join("vespera_spec-app.selected");
        let grace = Duration::from_secs(10 * 60);
        fs::write(&a, "{}").unwrap();
        fs::write(&b, "{}").unwrap();
        set_age(&a, Duration::from_secs(30 * 60));
        set_age(&b, Duration::from_secs(29 * 60));

        // `a` selected 30 minutes ago, `b` selected a minute later.
        prune_superseded_older_than(&a, stem, Duration::MAX);
        set_age(&marker, Duration::from_secs(30 * 60));
        prune_superseded_older_than(&b, stem, Duration::MAX);
        set_age(&marker, Duration::from_secs(29 * 60));
        // `a` is reselected without a rewrite, 2 minutes ago.
        prune_superseded_older_than(&a, stem, Duration::MAX);
        set_age(&marker, Duration::from_secs(2 * 60));
        prune_superseded_older_than(&a, stem, grace);
        assert!(b.exists(), "superseded just now, inside its grace period");

        set_age(&marker, Duration::from_secs(20 * 60));
        prune_superseded_older_than(&a, stem, grace);
        assert!(!b.exists());
        assert!(a.exists());
    }
}
