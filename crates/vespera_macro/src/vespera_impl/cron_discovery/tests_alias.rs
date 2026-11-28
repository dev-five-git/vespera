//! Sidecar of `tests.rs`: a job spelled through a local alias of vespera's
//! `cron` (`use vespera::cron as job;`) or of the vespera crate (`use vespera
//! as v;`) is found by the module-tree scan, not only spelled `cron` (round-41
//! R41-MR5-L1).

use rstest::rstest;

use super::tests::{crate_with, discover};
use crate::metadata::CronMetadata;

/// `(module, function, expression)` of each job, sorted.
fn placed(jobs: &[CronMetadata]) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = jobs
        .iter()
        .map(|job| {
            (
                job.module_path.clone(),
                job.function_name.clone(),
                job.expression.clone(),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn expected(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = rows
        .iter()
        .map(|(module, name, expr)| ((*module).to_owned(), (*name).to_owned(), (*expr).to_owned()))
        .collect();
    rows.sort();
    rows
}

#[rstest]
#[case::renamed_cron_import(
    r#"use vespera::cron as job; #[job("1 * * * * *")] pub async fn tick() {}"#,
    &[("", "tick", "1 * * * * *")]
)]
#[case::grouped_renamed_cron_import(
    r#"use vespera::{cron as job}; #[job("1 * * * * *")] pub async fn tick() {}"#,
    &[("", "tick", "1 * * * * *")]
)]
#[case::crate_alias(
    r#"use vespera as v; #[v::cron("1 * * * * *")] pub async fn tick() {}"#,
    &[("", "tick", "1 * * * * *")]
)]
#[case::alias_beats_a_foreign_glob(
    r#"use other::*; use vespera::cron as job; #[job("1 * * * * *")] pub async fn tick() {}"#,
    &[("", "tick", "1 * * * * *")]
)]
#[case::foreign_cron_alias_is_not_claimed(
    r#"use other::cron as job; #[job("1 * * * * *")] pub async fn tick() {}"#,
    &[]
)]
#[case::deeper_path_is_not_the_cron_macro(
    r#"use vespera::inner::cron as job; #[job("1 * * * * *")] pub async fn tick() {}"#,
    &[]
)]
#[case::longer_path_is_not_the_cron_macro(
    r#"#[a::b::cron("1 * * * * *")] pub async fn tick() {}"#,
    &[]
)]
#[case::unbound_crate_name_is_not_an_alias(
    r#"#[v::cron("1 * * * * *")] pub async fn tick() {}"#,
    &[]
)]
fn aliased_cron_attributes_are_claimed(
    #[case] source: &str,
    #[case] expected_jobs: &[(&str, &str, &str)],
) {
    let krate = crate_with(&[("lib.rs", source)]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(expected_jobs));
}

/// A `use super::*` / `use crate::*` glob carries the enclosing module's cron
/// alias into a child module (round-42 R42-MR5-M1), inline or in a file. A
/// child that binds the name itself, or that has no such glob, does not see it.
#[rstest]
#[case::inline_child_inherits_the_alias(
    r#"use vespera::cron as job; mod jobs { use super::*; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::crate_glob_inherits_the_alias(
    r#"use vespera::cron as job; mod jobs { use crate::*; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::crate_alias_is_inherited(
    r#"use vespera as v; mod jobs { use super::*; #[v::cron("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::grandchild_inherits_through_the_child(
    r#"use vespera::cron as job; mod a { use super::*; mod b { use super::*; #[job("1 * * * * *")] pub async fn tick() {} } }"#,
    &[("a::b", "tick", "1 * * * * *")]
)]
#[case::child_rebinding_the_name_is_not_a_job(
    r#"use vespera::cron as job; mod jobs { use super::*; use other::job; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
#[case::child_without_a_glob_does_not_see_the_alias(
    r#"use vespera::cron as job; mod jobs { #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
fn inherited_cron_aliases_reach_inline_child_modules(
    #[case] source: &str,
    #[case] expected_jobs: &[(&str, &str, &str)],
) {
    let krate = crate_with(&[("lib.rs", source)]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(expected_jobs));
}

/// The same inheritance into a file module, declared by name and by `#[path]`.
#[rstest]
#[case::by_name("mod jobs;", "jobs.rs")]
#[case::by_path("#[path = \"impl/jobs.rs\"] mod jobs;", "impl/jobs.rs")]
fn inherited_cron_alias_reaches_a_file_module(#[case] declaration: &str, #[case] file: &str) {
    let lib = format!("use vespera::cron as job;\n{declaration}\n");
    let krate = crate_with(&[
        ("lib.rs", lib.as_str()),
        (
            file,
            "use super::*;\n#[job(\"1 * * * * *\")]\npub async fn tick() {}\n",
        ),
    ]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(&[("jobs", "tick", "1 * * * * *")]));
}
