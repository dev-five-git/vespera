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
