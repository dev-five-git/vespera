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
//!   enough that no in-flight compilation can still be about to read them.
//!   The grace period of each file runs from the selection move that
//!   superseded it, which a `<stem>.selected` marker records per file (see
//!   `record_selection`); a later move does not postpone it, and a file
//!   reselected without a move keeps its time;
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
    collections::HashMap,
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long a superseded content-addressed file is kept for a parallel
/// compilation that may still `include_str!` it.
const PRUNE_AFTER: Duration = Duration::from_secs(10 * 60);

/// Most supersession records a `<stem>.selected` marker keeps. The oldest go
/// first; a file without a record is dated by the marker's mtime (the latest
/// move), which is never too early, so the log stays bounded without a
/// premature prune.
const SUPERSESSION_LOG_LIMIT: usize = 64;

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

/// The `<stem>.selected` marker. Its first line names the file `stem` last
/// selected. Each later line `<file> <unix secs>` records when `<file>` was
/// superseded by a selection move. A marker holding only a name (the format
/// before the log) has no such lines.
///
/// A move to `keep` appends the superseded file's record and drops the records
/// of `keep` itself (it is selected now). Rewritten only when the selection
/// moves: a kept file reselected without a move keeps the time it was
/// superseded at, not the reselection.
///
/// `None` when the marker could not be brought up to date: the supersession
/// times would then be unknown, so the caller prunes nothing this round.
fn record_selection(dir: &Path, stem: &str, keep: &Path) -> Option<PathBuf> {
    let marker = dir.join(format!("{stem}.selected"));
    let name = keep
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let previous = std::fs::read_to_string(&marker).unwrap_or_default();
    let mut lines = previous.lines();
    let current = lines.next().unwrap_or_default();
    if current == name {
        return Some(marker);
    }
    let now = unix_secs(SystemTime::now());
    let mut log: Vec<(String, u64)> = lines.filter_map(parse_record).collect();
    log.retain(|(file, _)| *file != name && file != current);
    if !current.is_empty() {
        log.push((current.to_owned(), now));
    }
    let excess = log.len().saturating_sub(SUPERSESSION_LOG_LIMIT);
    log.drain(..excess);
    let mut text = format!("{name}\n");
    for (file, secs) in &log {
        let _ = writeln!(text, "{file} {secs}");
    }
    std::fs::write(&marker, text).ok().map(|()| marker)
}

/// One `<file> <unix secs>` line of a marker's supersession log.
fn parse_record(line: &str) -> Option<(String, u64)> {
    let (file, secs) = line.rsplit_once(' ')?;
    Some((file.to_owned(), secs.parse().ok()?))
}

/// When each file in the marker's supersession log was superseded.
fn supersession_times(marker: &Path) -> HashMap<String, SystemTime> {
    std::fs::read_to_string(marker)
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(parse_record)
        .map(|(file, secs)| (file, UNIX_EPOCH + Duration::from_secs(secs)))
        .collect()
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn prune_superseded_older_than(keep: &Path, stem: &str, age: Duration) {
    let Some(dir) = keep.parent() else {
        return;
    };
    let (Some(marker), Ok(entries)) = (record_selection(dir, stem, keep), std::fs::read_dir(dir))
    else {
        return;
    };
    let prefix = format!("{stem}-");
    let now = SystemTime::now();
    let times = supersession_times(&marker);
    // A file the log does not name (an old marker, or a record dropped past
    // the limit) is dated by the marker's mtime, the latest move, or by its
    // own mtime when that is later: never too early.
    let last_move = std::fs::metadata(&marker)
        .and_then(|meta| meta.modified())
        .ok();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
        let stale = if is_temp_leftover(&name, stem) {
            // An interrupted write's temp file is superseded by nothing: only
            // its own age counts, so a live writer's temp file stays.
            modified.is_some_and(|modified| now.duration_since(modified).unwrap_or_default() >= age)
        } else {
            let is_sibling = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".json"))
                .is_some_and(|hash| {
                    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                });
            is_sibling && {
                let superseded_at = times.get(&name).copied().or(last_move.max(modified));
                superseded_at.is_some_and(|at| now.duration_since(at).unwrap_or_default() >= age)
            }
        };
        if stale && path != keep {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Whether `name` is a temp file of `write_atomic` for a `<stem>-<16 hex>.json`
/// sidecar (`.<stem>-<16 hex>.json.<pid>-<n>.tmp`), left behind when the
/// process died between the write and the rename.
fn is_temp_leftover(name: &str, stem: &str) -> bool {
    let Some(rest) = name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(stem))
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".tmp"))
    else {
        return false;
    };
    rest.get(..16)
        .is_some_and(|hash| hash.bytes().all(|b| b.is_ascii_hexdigit()))
        && rest[16..].starts_with(".json.")
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

    /// A selection whose marker cannot be written (here the marker path is a
    /// directory) prunes nothing this round: the superseded file is old, but
    /// the supersession time is unknown.
    #[test]
    fn failed_marker_write_skips_the_prune() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let keep = content_addressed(dir, "vespera_spec-app", 1);
        let old = content_addressed(dir, "vespera_spec-app", 2);
        fs::write(&keep, "{}").unwrap();
        fs::write(&old, "{}").unwrap();
        set_age(&old, Duration::from_secs(60 * 60));
        fs::create_dir(dir.join("vespera_spec-app.selected")).unwrap();

        prune_superseded_older_than(&keep, "vespera_spec-app", Duration::ZERO);
        assert!(old.exists());
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

        // A missing directory is a no-op, and so is a lone file with no
        // superseded siblings.
        prune_superseded(&dir.join("missing").join("x.json"), "x");
        let lone = content_addressed(dir, "lone", 9);
        fs::write(&lone, "{}").unwrap();
        prune_superseded(&lone, "lone");
        assert!(lone.exists());
    }

    /// A temp file a process left behind (killed between the write and the
    /// rename) is pruned once it is older than the age; a fresh one (a live
    /// writer) and a temp file of another stem are not this stem's leftovers.
    #[test]
    fn prune_removes_old_interrupted_temp_files_only() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let keep = content_addressed(dir, "vespera_spec-app", 1);
        fs::write(&keep, "{}").unwrap();
        let leftover = dir.join(".vespera_spec-app-0000000000000002.json.123-4.tmp");
        let live = dir.join(".vespera_spec-app-0000000000000003.json.123-5.tmp");
        let other = dir.join(".vespera_spec-app-x-0000000000000004.json.1-1.tmp");
        for path in [&leftover, &live, &other] {
            fs::write(path, "{}").unwrap();
        }
        for path in [&leftover, &other] {
            set_age(path, Duration::from_secs(60 * 60));
        }

        prune_superseded_older_than(&keep, "vespera_spec-app", Duration::from_secs(600));
        assert!(keep.exists());
        assert!(!leftover.exists());
        assert!(live.exists());
        assert!(other.exists());
    }

    fn set_age(path: &Path, ago: Duration) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - ago)
            .unwrap();
    }

    /// Sets when `file` was superseded, in the marker's log, to `ago` before
    /// now (a test's clock).
    fn age_record(marker: &Path, file: &Path, ago: Duration) {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let at = unix_secs(SystemTime::now() - ago);
        let mut text = String::new();
        for line in fs::read_to_string(marker).unwrap().lines() {
            match parse_record(line) {
                Some((record, _)) if record == name => {
                    let _ = writeln!(text, "{record} {at}");
                }
                _ => {
                    let _ = writeln!(text, "{line}");
                }
            }
        }
        fs::write(marker, text).unwrap();
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
    /// now survives its grace period, though `b` and `a` were both selected
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

        // `a` selected, then `b` selected (`a` superseded 30 minutes ago).
        prune_superseded_older_than(&a, stem, Duration::MAX);
        prune_superseded_older_than(&b, stem, Duration::MAX);
        age_record(&marker, &a, Duration::from_secs(30 * 60));
        // `a` is reselected, which supersedes `b` just now.
        prune_superseded_older_than(&a, stem, Duration::MAX);
        age_record(&marker, &b, Duration::from_secs(2 * 60));
        prune_superseded_older_than(&a, stem, grace);
        assert!(b.exists(), "superseded just now, inside its grace period");

        age_record(&marker, &b, Duration::from_secs(20 * 60));
        prune_superseded_older_than(&a, stem, grace);
        assert!(!b.exists());
        assert!(a.exists());
    }

    /// Several selection moves inside one grace window: each superseded file
    /// keeps its own record, so the marker's refreshed mtime (the latest move)
    /// does not postpone a file superseded longer ago.
    #[test]
    fn quick_moves_do_not_postpone_each_superseded_file() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let stem = "vespera_spec-app";
        let (a, b, c) = (
            content_addressed(dir, stem, 1),
            content_addressed(dir, stem, 2),
            content_addressed(dir, stem, 3),
        );
        let marker = dir.join("vespera_spec-app.selected");
        for file in [&a, &b, &c] {
            fs::write(file, "{}").unwrap();
            prune_superseded_older_than(file, stem, Duration::MAX);
        }
        age_record(&marker, &a, Duration::from_secs(30 * 60));
        age_record(&marker, &b, Duration::from_secs(5 * 60));
        let grace = Duration::from_secs(10 * 60);

        prune_superseded_older_than(&c, stem, grace);
        assert!(!a.exists(), "superseded 30 minutes ago");
        assert!(b.exists(), "superseded 5 minutes ago");
        assert!(c.exists());

        age_record(&marker, &b, Duration::from_secs(11 * 60));
        prune_superseded_older_than(&c, stem, grace);
        assert!(!b.exists());
        assert!(c.exists());
    }

    /// The log keeps at most [`SUPERSESSION_LOG_LIMIT`] records, the newest.
    #[test]
    fn supersession_log_stays_bounded() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let stem = "vespera_spec-app";
        let marker = dir.join("vespera_spec-app.selected");
        let moves = u64::try_from(SUPERSESSION_LOG_LIMIT + 10).unwrap();
        for hash in 1..=moves {
            let file = content_addressed(dir, stem, hash);
            fs::write(&file, "{}").unwrap();
            prune_superseded_older_than(&file, stem, Duration::MAX);
        }
        let text = fs::read_to_string(&marker).unwrap();
        assert_eq!(text.lines().count(), SUPERSESSION_LOG_LIMIT + 1, "{text}");
        let oldest = content_addressed(dir, stem, 1);
        let oldest = oldest.file_name().unwrap().to_string_lossy();
        assert!(!text.contains(oldest.as_ref()), "{text}");
    }

    /// A marker from before the log (one name, no newline) still selects: its
    /// superseded file is dated by the marker's mtime, and log lines that do
    /// not parse are dropped on the next move.
    #[test]
    fn old_single_line_marker_still_parses() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let stem = "vespera_spec-app";
        let keep = content_addressed(dir, stem, 1);
        let old = content_addressed(dir, stem, 2);
        let marker = dir.join("vespera_spec-app.selected");
        fs::write(&keep, "{}").unwrap();
        fs::write(&old, "{}").unwrap();
        fs::write(
            &marker,
            keep.file_name().unwrap().to_string_lossy().as_bytes(),
        )
        .unwrap();
        set_age(&marker, Duration::from_secs(60 * 60));
        set_age(&old, Duration::from_secs(2 * 60 * 60));

        prune_superseded_older_than(&keep, stem, Duration::from_secs(10 * 60));
        assert!(!old.exists());
        assert!(keep.exists());
    }

    #[test]
    fn log_lines_that_are_not_records_are_dropped() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let stem = "vespera_spec-app";
        let keep = content_addressed(dir, stem, 1);
        let next = content_addressed(dir, stem, 2);
        let marker = dir.join("vespera_spec-app.selected");
        fs::write(&keep, "{}").unwrap();
        fs::write(&marker, "stale\nnot a record\n\nfile-x 12x\n").unwrap();

        prune_superseded_older_than(&next, stem, Duration::MAX);
        let text = fs::read_to_string(&marker).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("stale "), "{text}");
    }

    /// A prune in a directory that has no marker yet creates it, naming `keep`.
    #[test]
    fn first_selection_writes_the_marker() {
        let temp = TempDir::new().unwrap();
        let keep = content_addressed(temp.path(), "vespera_spec-app", 1);
        fs::write(&keep, "{}").unwrap();
        prune_superseded(&keep, "vespera_spec-app");
        let text = fs::read_to_string(temp.path().join("vespera_spec-app.selected")).unwrap();
        assert_eq!(
            text,
            format!("{}\n", keep.file_name().unwrap().to_string_lossy())
        );
    }
}
