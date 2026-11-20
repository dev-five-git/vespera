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

/// A `#[cfg]`-gated module or fn is scanned: its job is registered under the
/// predicates of every item around it (`featured` gates `f` with `unix`), a
/// statically true gate drops out, and a statically false one (`any()`) is
/// never compiled, so its job is dropped.
#[test]
fn cfg_gated_modules_and_functions_register_their_jobs_under_their_gates() {
    let krate = crate_with(&[
        (
            "lib.rs",
            "#[cfg(unix)]\nmod unixy;\n#[cfg(all())]\nmod always;\n#[cfg(any())]\nmod never;\n\
             #[cfg(feature = \"x\")]\nmod featured;\n",
        ),
        ("unixy.rs", r#"#[cron("1 * * * * *")] pub async fn u() {}"#),
        (
            "always.rs",
            r#"#[cron("2 * * * * *")] pub async fn a() {}
            #[cfg(any())] #[cron("5 * * * * *")] pub async fn dead() {}"#,
        ),
        ("never.rs", r#"#[cron("3 * * * * *")] pub async fn n() {}"#),
        (
            "featured.rs",
            r#"#[cfg(unix)] #[cron("4 * * * * *")] pub async fn f() {}"#,
        ),
    ]);
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), Some(&src.join("lib.rs")));
    let mut found: Vec<(String, String, Vec<String>)> = jobs
        .iter()
        .map(|job| {
            (
                job.module_path.clone(),
                job.function_name.clone(),
                job.cfg.clone(),
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            ("always".to_owned(), "a".to_owned(), Vec::new()),
            (
                "featured".to_owned(),
                "f".to_owned(),
                vec!["feature = \"x\"".to_owned(), "unix".to_owned()]
            ),
            ("unixy".to_owned(), "u".to_owned(), vec!["unix".to_owned()]),
        ]
    );
}
