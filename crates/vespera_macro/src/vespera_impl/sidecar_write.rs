//! Crash- and race-safe writes of the files `include_str!` reads.
//!
//! A package's lib, bin and test targets expand the same `vespera!` /
//! `export_app!` in parallel `rustc` processes.  Their specs can differ (e.g.
//! `#[cfg(test)]` routes), so a shared file rewritten in place could be read
//! half-written or with the other target's content.  Two measures prevent it:
//!
//! * **atomic replace** — content goes to a unique temp file first and is
//!   renamed over the target, so a reader sees the old or the new file, never
//!   a torn one;
//! * **content addressing** — `include_str!` targets carry the content hash in
//!   their name, so each expansion embeds exactly the bytes it generated.
//!   Superseded files are pruned once they are old enough that no in-flight
//!   compilation can still be about to read them.

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
    std::fs::write(&temp, content)?;
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

fn prune_superseded_older_than(keep: &Path, stem: &str, age: Duration) {
    let Some(dir) = keep.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{stem}-");
    let now = SystemTime::now();
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
            .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() >= age);
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
}
