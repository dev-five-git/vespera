//! Sidecar of `tests.rs`: the `..` folding of absolute `#[path]` bases and the
//! registration gate of an undecided `cfg_attr(p, cron(..))`.

use std::path::Path;

use rstest::rstest;

use super::tests::{crate_with, discover};
use super::*;

/// The `..` of a `#[path]` child is folded before the file check, under a
/// verbatim-prefixed base too (where the filesystem leaves it unnormalised).
#[test]
fn fold_parent_dirs_drops_dots_and_pops_before_the_child() {
    let base = Path::new("//?/C:/proj/src");
    assert_eq!(
        fold_parent_dirs(&base.join("../shared/./jobs.rs")),
        Path::new("//?/C:/proj/shared/jobs.rs")
    );
    // Absolute bases only: a `..` with nothing left to pop is dropped.
    assert_eq!(
        fold_parent_dirs(Path::new("/a/../../b.rs")),
        Path::new("/").join("b.rs")
    );
    assert_eq!(fold_parent_dirs(Path::new("/./a.rs")), Path::new("/a.rs"));
    // `components()` keeps a `.` only in leading position (and under a
    // verbatim prefix on Windows), so this is the portable `CurDir` case.
    assert_eq!(fold_parent_dirs(Path::new("./a.rs")), Path::new("a.rs"));
}

/// A `cfg_attr(p, cron(..))` the scan cannot decide keeps its `p` as the job's
/// registration gate; a statically true `p` carries none.
#[rstest]
#[case::undecided("unix", vec!["unix".to_owned()])]
#[case::statically_true("all()", vec![])]
fn cfg_attr_cron_gate_is_the_undecided_predicate(
    #[case] predicate: &str,
    #[case] expected: Vec<String>,
) {
    let live =
        format!("#[cfg_attr({predicate}, cron(\"1 * * * * *\"))]\npub async fn tick() {{}}\n");
    let krate = crate_with(&[("lib.rs", "mod live;\n"), ("live.rs", live.as_str())]);
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), Some(&src.join("lib.rs")));
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].cfg, expected);
}

/// A plain `#[cron]` is never gated.
#[test]
fn plain_cron_has_no_registration_gate() {
    let krate = crate_with(&[(
        "lib.rs",
        "#[cron(\"1 * * * * *\")]\npub async fn tick() {}\n",
    )]);
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), Some(&src.join("lib.rs")));
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].cfg, Vec::<String>::new());
}
