//! Sidecar of `tests.rs`: the function index a stored job without an inline
//! module reads (round-37 M5-01). A file-level declaration, an inline-module
//! one and a statically dead one must not make such a job look ambiguous.

use super::tests::{crate_with, discover};
use crate::cron_impl::StoredCronInfo;
use crate::metadata::{CronMetadata, CronOptions};

/// A stored entry for `fn_name` in `file` that records no inline module.
fn stored_at(fn_name: &str, file: &std::path::Path, expression: &str) -> StoredCronInfo {
    StoredCronInfo {
        fn_name: fn_name.to_owned(),
        expression: expression.to_owned(),
        file_path: Some(file.display().to_string()),
        inline_module: None,
        options: CronOptions::default(),
    }
}

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

/// A file-level `tick` beside `mod a { tick }`: the inline-less stored entry
/// is the file's own job (module `""`), not an ambiguous pick between the two.
#[test]
fn stored_file_level_job_is_not_ambiguous_with_a_same_named_inline_module_fn() {
    let krate = crate_with(&[(
        "lib.rs",
        r#"
        #[cron("1 * * * * *")] pub async fn tick() {}
        pub mod a { #[cron("2 * * * * *")] pub async fn tick() {} }
        pub fn create_app() {}
        "#,
    )]);
    let src = krate.path().join("src");
    let lib = src.join("lib.rs");
    let storage = [stored_at("tick", &lib, "1 * * * * *")];
    let jobs = discover(&storage, Some(&src), Some(&lib));
    assert_eq!(
        placed(&jobs),
        expected(&[("", "tick", "1 * * * * *"), ("a", "tick", "2 * * * * *")])
    );
}

/// A statically dead `tick` never compiles, so the live one in `b` is the
/// only candidate for the inline-less stored entry (round-37 M5-01).
#[test]
fn statically_dead_same_named_declaration_does_not_make_a_stored_job_ambiguous() {
    let krate = crate_with(&[(
        "lib.rs",
        r#"
        #[cfg(any())] pub mod a { #[cron("1 * * * * *")] pub async fn tick() {} }
        pub mod b { #[cron("2 * * * * *")] pub async fn tick() {} }
        pub fn create_app() {}
        "#,
    )]);
    let src = krate.path().join("src");
    let lib = src.join("lib.rs");
    let storage = [stored_at("tick", &lib, "2 * * * * *")];
    let jobs = discover(&storage, Some(&src), Some(&lib));
    assert_eq!(placed(&jobs), expected(&[("b", "tick", "2 * * * * *")]));
}

/// The only same-named function is in one inline module: the inline-less
/// stored entry is placed there, as before the file-level index split.
#[test]
fn inline_less_stored_job_of_a_single_inline_module_function_is_placed_there() {
    let krate = crate_with(&[(
        "lib.rs",
        r#"
        pub mod a { #[cron("2 * * * * *")] pub async fn tick() {} }
        pub fn create_app() {}
        "#,
    )]);
    let src = krate.path().join("src");
    let lib = src.join("lib.rs");
    let storage = [stored_at("tick", &lib, "2 * * * * *")];
    let jobs = discover(&storage, Some(&src), Some(&lib));
    assert_eq!(placed(&jobs), expected(&[("a", "tick", "2 * * * * *")]));
}
