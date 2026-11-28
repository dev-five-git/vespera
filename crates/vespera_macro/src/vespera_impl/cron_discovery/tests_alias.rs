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

/// A path of three or more segments is never vespera's `cron`, whatever the
/// imports bind (the scan itself only passes paths of at most two segments
/// on, see `cron_candidates`; this pins the wildcard arm directly).
#[test]
fn a_path_of_three_segments_is_never_the_cron_macro() {
    use super::imports::CronImports;
    let imports = CronImports::default();
    let segments: Vec<String> = ["vespera", "inner", "cron"]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(!imports.is_cron_segments(&segments));
}

/// `use crate::*;` brings the crate ROOT's imports into a module at any depth,
/// not the enclosing module's (round-43 R43-MR5-M1). `use super::*;` keeps the
/// enclosing module's: the root's alias does not reach a grandchild through
/// its parent that does not import it.
#[rstest]
#[case::crate_glob_reaches_the_root_through_a_module(
    r#"use vespera::cron as job; mod a { mod b { use crate::*; #[job("1 * * * * *")] pub async fn tick() {} } }"#,
    &[("a::b", "tick", "1 * * * * *")]
)]
#[case::super_glob_does_not_reach_past_its_parent(
    r#"use vespera::cron as job; mod a { mod b { use super::*; #[job("1 * * * * *")] pub async fn tick() {} } }"#,
    &[]
)]
#[case::crate_glob_reaches_the_root_crate_alias(
    r#"use vespera as v; mod a { mod b { use crate::*; #[v::cron("1 * * * * *")] pub async fn tick() {} } }"#,
    &[("a::b", "tick", "1 * * * * *")]
)]
#[case::crate_glob_keeps_the_root_foreign_cron(
    r#"use other::cron; mod a { mod b { use crate::*; #[cron("1 * * * * *")] pub async fn tick() {} } }"#,
    &[]
)]
fn crate_glob_inherits_the_crate_root_imports(
    #[case] source: &str,
    #[case] expected_jobs: &[(&str, &str, &str)],
) {
    let krate = crate_with(&[("lib.rs", source)]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(expected_jobs));
}

/// The R43-MR5-M1 scenario across a file module: `lib.rs` binds the alias and
/// declares `mod a;`, whose inline `mod b` globs the crate root.
#[test]
fn crate_glob_in_a_file_module_reaches_the_root_alias() {
    let krate = crate_with(&[
        ("lib.rs", "use vespera::cron as job;\nmod a;\n"),
        (
            "a.rs",
            "pub mod b { use crate::*; #[job(\"1 * * * * *\")] pub async fn tick() {} }\n",
        ),
    ]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(&[("a::b", "tick", "1 * * * * *")]));
}

/// A child module that imports the parent's cron alias by name (`use
/// super::job;`, `use super::job as j;`, `use crate::job;`) binds it, so its
/// `#[job]` is scheduled (round-45 R45-MR5-M1), inline or in a file.
#[rstest]
#[case::inline_by_name(
    r#"use vespera::cron as job; mod jobs { use super::job; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::inline_renamed(
    r#"use vespera::cron as job; mod jobs { use super::job as j; #[j("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::inline_from_the_crate_root(
    r#"use vespera::cron as job; mod jobs { use crate::job; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::foreign_alias_is_not_inherited_by_name(
    r#"use other::cron as job; mod jobs { use super::job; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
#[case::unknown_name_is_not_an_alias(
    r#"use vespera::cron; mod jobs { use super::missing; #[missing("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
fn parent_alias_imported_by_name_reaches_the_child(
    #[case] source: &str,
    #[case] expected_jobs: &[(&str, &str, &str)],
) {
    let krate = crate_with(&[("lib.rs", source)]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(expected_jobs));
}

/// The same by-name import from a file module (`mod jobs;`).
#[rstest]
#[case::by_name("use super::job;", "job")]
#[case::renamed("use super::job as j;", "j")]
#[case::from_the_crate_root("use crate::job;", "job")]
fn parent_alias_imported_by_name_reaches_a_file_module(#[case] import: &str, #[case] attr: &str) {
    let krate = crate_with(&[
        ("lib.rs", "use vespera::cron as job;\nmod jobs;\n"),
        (
            "jobs.rs",
            &format!("{import}\n#[{attr}(\"1 * * * * *\")]\npub async fn tick() {{}}\n"),
        ),
    ]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(&[("jobs", "tick", "1 * * * * *")]));
}

/// `use super::cron;` / `use crate::cron;` bind the parent's (or the crate
/// root's) bare `cron`, so the child's bare `#[cron]` is the same macro: a
/// foreign one is not vespera's (round-45 R45-MR5-L1), a vespera one is.
#[rstest]
#[case::foreign_parent_cron(
    r#"use other::cron; mod jobs { use super::cron; #[cron("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
#[case::foreign_parent_cron_renamed(
    r#"use other::cron; mod jobs { use super::cron as job; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
#[case::vespera_parent_cron(
    r#"use vespera::cron; mod jobs { use super::cron; #[cron("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::vespera_parent_cron_renamed_to_itself(
    r#"use vespera::cron; mod jobs { use super::cron as cron; #[cron("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::vespera_parent_cron_renamed(
    r#"use vespera::cron; mod jobs { use super::cron as job; #[job("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
#[case::foreign_crate_root_cron(
    r#"use other::cron; mod jobs { use crate::cron; #[cron("1 * * * * *")] pub async fn tick() {} }"#,
    &[]
)]
#[case::vespera_crate_root_cron(
    r#"use vespera::cron; mod jobs { use crate::cron; #[cron("1 * * * * *")] pub async fn tick() {} }"#,
    &[("jobs", "tick", "1 * * * * *")]
)]
fn inherited_bare_cron_keeps_the_parents_foreign_state(
    #[case] source: &str,
    #[case] expected_jobs: &[(&str, &str, &str)],
) {
    let krate = crate_with(&[("lib.rs", source)]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(expected_jobs));
}

/// A crate alias bound in the parent (`use vespera as v;`) reaches a child that
/// imports it by name (`use super::v;`), so the child's `#[v::cron]` is a job.
#[test]
fn parent_crate_alias_imported_by_name_reaches_the_child() {
    let krate = crate_with(&[(
        "lib.rs",
        r#"use vespera as v; mod jobs { use super::v; #[v::cron("1 * * * * *")] pub async fn tick() {} }"#,
    )]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(placed(&jobs), expected(&[("jobs", "tick", "1 * * * * *")]));
}
