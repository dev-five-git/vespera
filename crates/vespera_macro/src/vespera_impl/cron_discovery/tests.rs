use std::fs;

use rstest::rstest;
use tempfile::TempDir;

use crate::metadata::CronOptions;

use super::*;

fn discover(
    storage: &[StoredCronInfo],
    src_dir: Option<&Path>,
    call_site: Option<&Path>,
) -> Vec<CronMetadata> {
    discover_cron_jobs(storage, src_dir, call_site).unwrap()
}

/// `vespera!` in an example / integration test (outside `src/`): the
/// call-site file is the crate root, its `mod`s resolve next to it, and
/// jobs there — stored or not — get real module paths.
#[test]
fn call_site_outside_src_is_the_crate_root() {
    let krate = crate_with(&[("lib.rs", "")]);
    let examples = krate.path().join("examples");
    fs::create_dir_all(examples.join("demo")).unwrap();
    let root = examples.join("demo").join("main.rs");
    fs::write(
        &root,
        r#"mod jobs; #[cron("1 * * * * *")] pub async fn root_job() {}"#,
    )
    .unwrap();
    let jobs_file = examples.join("demo").join("jobs.rs");
    fs::write(
        &jobs_file,
        r#"#[cron("2 * * * * *")] pub async fn tick() {} #[cron("3 * * * * *")] pub async fn late() {}"#,
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    let storage = [stored("tick", Some(jobs_file.display().to_string()))];
    let jobs = discover(&storage, Some(&src), Some(&root));
    assert_eq!(
        summary(&jobs),
        rows(&[
            ("", "root_job", "1 * * * * *"),
            ("jobs", "late", "3 * * * * *"),
            ("jobs", "tick", "0 0 0 * * *"),
        ])
    );
    // Without a tree to consult the stored file is still placed relative
    // to the call site (here: `examples/demo` is not under `src/`).
    let placement = Placement {
        src_dir: Some(&src),
        tree: None,
        call_site: Some(canonical_display(&root)),
    };
    let placed = stored_jobs(
        &[
            stored("tick", Some(jobs_file.display().to_string())),
            stored("root_job", Some(root.display().to_string())),
        ],
        &placement,
        &mut HashMap::new(),
    )
    .unwrap();
    assert_eq!(
        summary(&placed),
        rows(&[
            ("", "root_job", "0 0 0 * * *"),
            ("jobs", "tick", "0 0 0 * * *")
        ])
    );
}

/// A job in a module of a call site outside `src/` is found even when it
/// expands after `vespera!` (no stored job) and neither the call-site file
/// nor `src/` mentions `cron(`.
#[test]
fn unexpanded_job_in_a_module_of_a_call_site_outside_src() {
    let krate = crate_with(&[("lib.rs", "")]);
    let demo = krate.path().join("examples").join("demo");
    fs::create_dir_all(&demo).unwrap();
    let root = demo.join("main.rs");
    fs::write(&root, "mod jobs; fn main() {}").unwrap();
    fs::write(
        demo.join("jobs.rs"),
        r#"#[vespera::cron("2 * * * * *")] pub async fn tick() {}"#,
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let jobs = discover(&[], Some(&krate.path().join("src")), Some(&root));
    assert_eq!(summary(&jobs), rows(&[("jobs", "tick", "2 * * * * *")]));
}

/// A `#[path]` module cycle ends the walk instead of overflowing the stack.
#[test]
fn module_cycle_is_walked_once() {
    let krate = crate_with(&[(
        "lib.rs",
        r#"#[path = "lib.rs"] mod again; #[cron("1 * * * * *")] pub async fn tick() {}"#,
    )]);
    let jobs = discover(&[], Some(&krate.path().join("src")), None);
    assert_eq!(summary(&jobs), rows(&[("", "tick", "1 * * * * *")]));
}

/// A stored job whose file is neither under `src/` nor next to the call
/// site has no module path: a compile error, not an absolute-path module.
#[test]
fn stored_job_outside_the_crate_is_an_error() {
    let krate = crate_with(&[("lib.rs", "")]);
    let elsewhere = TempDir::new().unwrap();
    let stray = elsewhere.path().join("stray.rs");
    fs::write(&stray, r#"#[cron("1 * * * * *")] pub async fn stray() {}"#).unwrap();
    let src = krate.path().join("src");
    let storage = [stored("stray", Some(stray.display().to_string()))];
    for call_site in [Some(src.join("lib.rs")), None] {
        let error = discover_cron_jobs(&storage, Some(&src), call_site.as_deref()).unwrap_err();
        assert!(
            error.contains("cannot place `#[cron]` job `stray`"),
            "unexpected error: {error}"
        );
    }
    // A sibling directory sharing the `src` prefix is not `src/`.
    let src2 = krate.path().join("src2");
    fs::create_dir_all(&src2).unwrap();
    let sibling = src2.join("job.rs");
    fs::write(&sibling, "").unwrap();
    let placement = Placement {
        src_dir: Some(&src),
        tree: None,
        call_site: Some(canonical_display(&src.join("lib.rs"))),
    };
    assert!(
        stored_jobs(
            &[stored("job", Some(sibling.display().to_string()))],
            &placement,
            &mut HashMap::new()
        )
        .is_err()
    );
}

/// A custom `[lib] path = "src/api.rs"`: the crate root rustc compiles is
/// the call-site file, so its jobs are crate-root jobs (`crate::tick`, not
/// `crate::api::tick`) and jobs in its child modules are still scanned.
#[test]
fn custom_crate_root_under_src_is_read_from_the_compiler_args() {
    let krate = crate_with(&[
        (
            "api.rs",
            r#"mod jobs; #[cron("1 * * * * *")] pub async fn tick() {}"#,
        ),
        (
            "jobs.rs",
            r#"#[cron("2 * * * * *")] pub async fn late() {}"#,
        ),
    ]);
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    let root = src.join("api.rs");
    let args = format!(
        "--crate-name
api
--edition=2024
{}",
        root.display()
    );
    let _args = crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, args);
    let storage = [stored("tick", Some(root.display().to_string()))];
    assert_eq!(
        summary(&discover(&storage, Some(&src), Some(&root))),
        rows(&[("", "tick", "0 0 0 * * *"), ("jobs", "late", "2 * * * * *")])
    );
    // A compiler root whose tree does not contain the call site is ignored.
    let elsewhere = src.join("jobs.rs");
    assert!(
        compiler_root_tree(
            &[root.display().to_string()],
            &canonical_display(&elsewhere)
        )
        .is_some()
    );
    assert!(
        compiler_root_tree(
            &[elsewhere.display().to_string()],
            &canonical_display(&root)
        )
        .is_none()
    );
    assert!(compiler_root_tree(&["--test".to_owned()], &canonical_display(&root)).is_none());
}

/// A module shared by the library and the binary (`mod app;` in both
/// roots) is in both trees: the binary build (compiler args name
/// `src/main.rs`) scans the binary's tree, never the library's `mod jobs;`.
#[test]
fn shared_module_scans_the_tree_rustc_compiles() {
    let krate = crate_with(&[
        ("lib.rs", "mod app; mod jobs;"),
        ("main.rs", "mod app; fn main() {}"),
        ("app.rs", "pub fn create_app() {}"),
        (
            "jobs.rs",
            r#"#[cron("1 * * * * *")] pub async fn tick() {}"#,
        ),
    ]);
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    let app = src.join("app.rs");
    {
        let main = src.join("main.rs").display().to_string();
        let _args =
            crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, main);
        assert!(discover(&[], Some(&src), Some(&app)).is_empty());
    }
    let lib = src.join("lib.rs").display().to_string();
    let _args = crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, lib);
    assert_eq!(
        summary(&discover(&[], Some(&src), Some(&app))),
        rows(&[("jobs", "tick", "1 * * * * *")])
    );
}

/// `#[path = "dir"] mod m { mod jobs; }` resolves `jobs` under `dir`, as
/// rustc does (from a mod-rs root and from a non-mod-rs file alike).
#[test]
fn inline_path_module_resolves_its_children_under_the_path() {
    let krate = crate_with(&[
        (
            "lib.rs",
            r#"mod nested; #[path = "dir"] mod m { mod jobs; }"#,
        ),
        (
            "dir/jobs.rs",
            r#"#[cron("1 * * * * *")] pub async fn tick() {}"#,
        ),
        (
            "m/jobs.rs",
            r#"#[cron("9 * * * * *")] pub async fn wrong() {}"#,
        ),
        ("nested.rs", r#"#[path = "inner"] mod n { mod deep; }"#),
        (
            "nested/inner/deep.rs",
            r#"#[cron("2 * * * * *")] pub async fn deep() {}"#,
        ),
    ]);
    crate::schema_macro::file_cache::bump_epoch();
    let src = krate.path().join("src");
    assert_eq!(
        summary(&discover(&[], Some(&src), Some(&src.join("lib.rs")))),
        rows(&[
            ("m::jobs", "tick", "1 * * * * *"),
            ("nested::n::deep", "deep", "2 * * * * *"),
        ])
    );
}

/// Write `files` (relative to `src/`) into a fresh temp crate and return it.
fn crate_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("temp dir");
    for (relative, content) in files {
        let path = dir.path().join("src").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    dir
}

fn stored(fn_name: &str, file_path: Option<String>) -> StoredCronInfo {
    StoredCronInfo {
        fn_name: fn_name.to_owned(),
        expression: "0 0 0 * * *".to_owned(),
        file_path,
        inline_module: None,
        options: CronOptions::default(),
    }
}

fn stored_in(fn_name: &str, file_path: &Path, inline: &str, expr: &str) -> StoredCronInfo {
    StoredCronInfo {
        expression: expr.to_owned(),
        inline_module: Some(inline.to_owned()),
        ..stored(fn_name, Some(file_path.display().to_string()))
    }
}

/// Same-named jobs in different inline modules of one file: each stored
/// entry is placed in its own module, and a sibling the storage does not
/// know yet is still scanned.  A `#[path]` file's jobs take the declaring
/// module's path plus the recorded inline module.
#[test]
fn same_named_jobs_in_inline_modules_do_not_clash() {
    let krate = crate_with(&[
        (
            "lib.rs",
            r#"
            pub mod a { #[cron("1 * * * * *")] pub async fn tick() {} }
            pub mod b { #[cron("2 * * * * *")] pub async fn tick() {} }
            pub mod c { pub mod d { #[cron("3 * * * * *")] pub async fn tick() {} } }
            #[path = "x.rs"] mod pathed;
            "#,
        ),
        (
            "x.rs",
            r#"mod e { #[cron("4 * * * * *")] pub async fn tick() {} }"#,
        ),
    ]);
    let src = krate.path().join("src");
    let lib = src.join("lib.rs");
    let storage = [
        stored_in("tick", &lib, "a", "1 * * * * *"),
        stored_in("tick", &lib, "b", "2 * * * * *"),
        stored_in("tick", &src.join("x.rs"), "e", "4 * * * * *"),
    ];
    let jobs = discover(&storage, Some(&src), Some(&lib));
    assert_eq!(
        summary(&jobs),
        rows(&[
            ("a", "tick", "1 * * * * *"),
            ("b", "tick", "2 * * * * *"),
            ("c::d", "tick", "3 * * * * *"),
            ("pathed::e", "tick", "4 * * * * *"),
        ])
    );
}

fn summary(jobs: &[CronMetadata]) -> Vec<(String, String, String)> {
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

#[rstest]
#[case::slashes_and_hyphens("admin-tools/daily-job.rs", "admin_tools::daily_job")]
#[case::module_file("admin-tools/mod.rs", "admin_tools")]
#[case::library_root("lib.rs", "")]
#[case::binary_root("main.rs", "")]
#[case::nested_lib_file("tools/lib.rs", "tools::lib")]
fn cron_module_path_normalizes_rust_module_syntax(#[case] input: &str, #[case] expected: &str) {
    assert_eq!(cron_module_path(input), expected);
}

/// Without a known `src/`, an absolute file is no module path: placement is
/// an error, not a path made of the absolute directories.
#[test]
fn absolute_storage_path_without_src_dir_is_a_placement_error() {
    let absolute = std::env::temp_dir()
        .join("vespera-abs-proj")
        .join("jobs.rs");
    let storage = [stored(
        "tick",
        Some(absolute.to_string_lossy().into_owned()),
    )];
    let err = discover_cron_jobs(&storage, None, None)
        .expect_err("an absolute path must not become a module path");
    assert!(err.contains("`#[cron]` job `tick`"), "{err}");
}

#[test]
fn storage_entries_without_src_dir_keep_their_derived_paths() {
    let jobs = discover(
        &[
            stored("no_file", None),
            stored("missing", Some("does/not/exist.rs".to_owned())),
        ],
        None,
        None,
    );
    assert_eq!(
        summary(&jobs),
        vec![
            (
                String::new(),
                "no_file".to_owned(),
                "0 0 0 * * *".to_owned()
            ),
            (
                "does::not::exist".to_owned(),
                "missing".to_owned(),
                "0 0 0 * * *".to_owned()
            ),
        ]
    );
}

/// Jobs in modules rustc expands after `vespera!` are recovered from the
/// module tree (`#[path]` modules included); storage-known, gated, private,
/// non-literal and unreachable ones are not.
/// `#[cron ("..")]` is valid Rust: its file is still scanned, so the job
/// is found although no file spells `cron(` with no space.
#[test]
fn scan_finds_a_cron_attribute_with_a_space_before_its_parenthesis() {
    let krate = crate_with(&[
        ("lib.rs", "mod late;\npub fn create_app() {}\n"),
        (
            "late.rs",
            "#[cron (\"1 * * * * *\")]\npub async fn tick() {}\n",
        ),
    ]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(
        summary(&jobs),
        vec![(
            "late".to_owned(),
            "tick".to_owned(),
            "1 * * * * *".to_owned()
        )]
    );
}

#[test]
fn scan_recovers_unexpanded_jobs_from_the_module_tree() {
    let krate = crate_with(&[
        (
            "lib.rs",
            r#"
            mod early;
            pub fn create_app() {}
            mod late;
            mod plain;
            mod leafy;
            mod broken;
            mod missing;
            #[cfg(test)] mod gated;
            #[path = "x.rs"] mod pathed;
            pub mod inline {
                #[vespera::cron("0 0 * * * *")]
                pub async fn inline_job() {}
            }
            "#,
        ),
        (
            "early.rs",
            r#"#[cron("1 * * * * *")] pub async fn early_job() {}"#,
        ),
        (
            "late/mod.rs",
            r#"
            mod deeper;
            #[cron("2 * * * * *", overlap = "skip", tz = "Asia/Seoul")] pub async fn late_job() {}
            #[cron("3 * * * * *", bogus = "x")] pub async fn bad_args() {}
            #[cron("3 * * * * *")] fn private_job() {}
            #[cfg(feature = "x")] #[cron("4 * * * * *")] pub async fn gated_job() {}
            #[cron(EXPR)] pub async fn non_literal() {}
            #[other] pub async fn not_cron() {}
            #[a::cron("9 * * * * *")] pub async fn foreign_cron() {}
            "#,
        ),
        (
            "late/deeper.rs",
            r#"#[vespera::cron("5 * * * * *")] pub async fn deep_job() {}"#,
        ),
        ("plain.rs", "pub mod inner { pub async fn helper() {} }"),
        ("leafy.rs", "pub fn leaf() {}"),
        ("broken.rs", "mod {{{ cron("),
        (
            "gated.rs",
            r#"#[cron("6 * * * * *")] pub async fn gated() {}"#,
        ),
        ("x.rs", r#"#[cron("7 * * * * *")] pub async fn pathed() {}"#),
        (
            "stray.rs",
            r#"#[cron("8 * * * * *")] pub async fn stray() {}"#,
        ),
    ]);
    let src = krate.path().join("src");
    let early = src.join("early.rs").display().to_string();
    let jobs = discover(&[stored("early_job", Some(early))], Some(&src), None);
    assert_eq!(
        summary(&jobs),
        vec![
            (
                "early".to_owned(),
                "early_job".to_owned(),
                "0 0 0 * * *".to_owned()
            ),
            (
                "inline".to_owned(),
                "inline_job".to_owned(),
                "0 0 * * * *".to_owned()
            ),
            (
                "late".to_owned(),
                "late_job".to_owned(),
                "2 * * * * *".to_owned()
            ),
            (
                "late::deeper".to_owned(),
                "deep_job".to_owned(),
                "5 * * * * *".to_owned()
            ),
            (
                "pathed".to_owned(),
                "pathed".to_owned(),
                "7 * * * * *".to_owned()
            ),
        ]
    );
    // Scanned jobs keep their `tz` / `overlap` arguments.
    let late = jobs
        .iter()
        .find(|job| job.function_name == "late_job")
        .unwrap();
    assert_eq!(
        late.options,
        CronOptions {
            timezone: Some("Asia/Seoul".to_owned()),
            skip_overlap: true,
        }
    );
}

/// Without a `cron(` attribute outside the storage-covered files nothing
/// is parsed and nothing is added.
#[rstest]
#[case::no_cron_anywhere(&[("lib.rs", "mod a;"), ("a.rs", "pub fn a() {}")], false)]
#[case::all_in_storage(&[("lib.rs", "mod a;"), ("a.rs", r#"#[cron("1 * * * * *")] pub async fn a() {}"#)], true)]
fn scan_adds_nothing_without_unexpanded_candidates(
    #[case] files: &[(&str, &str)],
    #[case] store_a: bool,
) {
    let krate = crate_with(files);
    let src = krate.path().join("src");
    let storage: Vec<_> = store_a
        .then(|| stored("a", Some(src.join("a.rs").display().to_string())))
        .into_iter()
        .collect();
    let jobs = discover(&storage, Some(&src), None);
    assert_eq!(jobs.len(), storage.len());
}

/// Only the module tree holding the `vespera!` call site contributes.
#[rstest]
#[case::library_call_site(Some("lib.rs"), vec!["lib_job"])]
#[case::binary_call_site(Some("main.rs"), vec!["bin_job"])]
#[case::outside_both_trees(Some("orphan.rs"), vec![])]
#[case::binary_target(Some("bin/server.rs"), vec!["server_tasks_job", "server_job"])]
#[case::binary_dir_target(Some("bin/worker/main.rs"), vec!["worker_job"])]
#[case::unknown_call_site_prefers_library(None, vec!["lib_job"])]
fn scan_uses_the_tree_containing_the_call_site(
    #[case] call_site: Option<&str>,
    #[case] expected: Vec<&str>,
) {
    let krate = crate_with(&[
        (
            "lib.rs",
            r#"#[cron("1 * * * * *")] pub async fn lib_job() {}"#,
        ),
        (
            "main.rs",
            r#"#[cron("2 * * * * *")] pub async fn bin_job() {}"#,
        ),
        ("orphan.rs", ""),
        (
            "bin/server.rs",
            r#"mod tasks; #[cron("3 * * * * *")] pub async fn server_job() {}"#,
        ),
        (
            "bin/tasks.rs",
            r#"#[cron("4 * * * * *")] pub async fn server_tasks_job() {}"#,
        ),
        (
            "bin/worker/main.rs",
            r#"#[cron("5 * * * * *")] pub async fn worker_job() {}"#,
        ),
        ("bin/README.md", "not a crate root"),
    ]);
    let src = krate.path().join("src");
    let call_site = call_site.map(|file| src.join(file));
    let jobs = discover(&[], Some(&src), call_site.as_deref());
    let names: Vec<_> = jobs.iter().map(|job| job.function_name.as_str()).collect();
    assert_eq!(names, expected);
}

/// A job stored from a binary's root file lives at that crate's root,
/// not at `crate::bin::server`.
#[test]
fn binary_root_jobs_are_placed_at_the_crate_root() {
    let krate = crate_with(&[(
        "bin/server.rs",
        r#"#[cron("1 * * * * *")] pub async fn tick() {}"#,
    )]);
    let src = krate.path().join("src");
    let server = src.join("bin").join("server.rs");
    let storage = [stored("tick", Some(server.display().to_string()))];
    let jobs = discover(&storage, Some(&src), Some(&server));
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].module_path, "");
}

#[test]
fn crate_without_a_root_file_contributes_nothing() {
    let krate = crate_with(&[("jobs.rs", r#"#[cron("1 * * * * *")] pub async fn job() {}"#)]);
    let src = krate.path().join("src");
    assert!(discover(&[], Some(&src), None).is_empty());
}

/// A bare `#[cron]` that a module imports from another crate is not
/// vespera's: it is never scheduled.  `vespera::cron` always is, and an
/// import from vespera keeps the bare form vespera's.
#[rstest]
#[case::foreign_import("use other::cron;", &[("", "spelled", "2 * * * * *")])]
#[case::foreign_group("use other::{cron, x};", &[("", "spelled", "2 * * * * *")])]
#[case::foreign_rename("use other::schedule as cron;", &[("", "spelled", "2 * * * * *")])]
#[case::vespera_import("use vespera::cron;", &[("", "bare", "1 * * * * *"), ("", "spelled", "2 * * * * *")])]
#[case::vespera_group("use ::vespera::{cron, axum};", &[("", "bare", "1 * * * * *"), ("", "spelled", "2 * * * * *")])]
#[case::glob_import("use other::*;", &[("", "bare", "1 * * * * *"), ("", "spelled", "2 * * * * *")])]
#[case::other_cron_name("use other::cron as tick;", &[("", "bare", "1 * * * * *"), ("", "spelled", "2 * * * * *")])]
fn foreign_cron_attributes_are_not_claimed(
    #[case] import: &str,
    #[case] expected: &[(&str, &str, &str)],
) {
    let source = format!(
        r#"{import} #[cron("1 * * * * *")] pub async fn bare() {{}} #[vespera::cron("2 * * * * *")] pub async fn spelled() {{}}"#
    );
    let krate = crate_with(&[("lib.rs", &source)]);
    let src = krate.path().join("src");
    let jobs = discover(&[], Some(&src), None);
    assert_eq!(summary(&jobs), rows(expected));
}

fn rows(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = rows
        .iter()
        .map(|(module, name, expr)| ((*module).to_owned(), (*name).to_owned(), (*expr).to_owned()))
        .collect();
    rows.sort();
    rows
}

/// Stored jobs are placed by the module tree: the crate root is `crate`
/// (not `crate::lib`), inline modules keep their segments (also
/// `#[cfg]`-gated ones rustc expanded), `#[path]` modules are followed, and
/// a job declared after `vespera!` in a file with stored jobs is still
/// scanned.
#[test]
fn stored_jobs_take_their_module_path_from_the_module_tree() {
    let krate = crate_with(&[
        (
            "lib.rs",
            r#"
            #[vespera::cron("1 * * * * *")] pub async fn root_job() {}
            pub mod jobs {
                #[cron("2 * * * * *")] pub async fn inline_job() {}
                pub mod deeper {
                    #[cron("3 * * * * *")] pub async fn deep_job() {}
                }
                #[path = "y.rs"] mod inner_pathed;
            }
            #[path = "x.rs"] mod pathed;
            #[path = "missing.rs"] mod absent;
            pub fn create_app() {}
            #[cron("4 * * * * *")] pub async fn after_job() {}
            #[cfg(test)]
            mod tests {
                #[cron("5 * * * * *")] pub async fn gated_job() {}
            }
            "#,
        ),
        (
            "x.rs",
            r#"#[cron("6 * * * * *")] pub async fn pathed_job() {} mod child;"#,
        ),
        (
            "child.rs",
            r#"#[cron("8 * * * * *")] pub async fn child_job() {}"#,
        ),
        (
            "jobs/y.rs",
            r#"#[cron("7 * * * * *")] pub async fn late_pathed() {}"#,
        ),
    ]);
    let src = krate.path().join("src");
    let lib = src.join("lib.rs").display().to_string();
    let x = src.join("x.rs").display().to_string();
    let storage = [
        stored("root_job", Some(lib.clone())),
        stored("inline_job", Some(lib.clone())),
        stored("deep_job", Some(lib.clone())),
        stored("gated_job", Some(lib)),
        stored("pathed_job", Some(x)),
    ];
    let stored_expr = "0 0 0 * * *";
    let jobs = discover(&storage, Some(&src), Some(&src.join("lib.rs")));
    assert_eq!(
        summary(&jobs),
        rows(&[
            ("", "root_job", stored_expr),
            ("jobs", "inline_job", stored_expr),
            ("jobs::deeper", "deep_job", stored_expr),
            ("tests", "gated_job", stored_expr),
            // `#[path]` modules are followed like rustc: file level relative
            // to the declaring file, inside inline modules relative to their
            // directory, the loaded file's children next to it.
            ("pathed", "pathed_job", stored_expr),
            ("jobs::inner_pathed", "late_pathed", "7 * * * * *"),
            ("pathed::child", "child_job", "8 * * * * *"),
            ("", "after_job", "4 * * * * *"),
        ])
    );
}

/// Without a module tree to consult (call site outside both trees), a
/// stored crate-root job still resolves to `crate`.
#[test]
fn stored_root_job_without_a_tree_falls_back_to_the_crate_root() {
    let krate = crate_with(&[
        (
            "main.rs",
            r#"#[cron("1 * * * * *")] pub async fn bin_job() {}"#,
        ),
        ("orphan.rs", ""),
    ]);
    let src = krate.path().join("src");
    let main = src.join("main.rs").display().to_string();
    let jobs = discover(
        &[stored("bin_job", Some(main))],
        Some(&src),
        Some(&src.join("orphan.rs")),
    );
    assert_eq!(summary(&jobs), rows(&[("", "bin_job", "0 0 0 * * *")]));
}

/// Inside rust-analyzer the storage of one package is shared by its targets:
/// a stored job of another target (here `examples/demo.rs`) is dropped from
/// the library's expansion instead of failing it, while the library's own
/// stored job stays.  Under rustc nothing is filtered.
#[test]
fn stored_jobs_of_another_target_are_dropped_in_the_ide() {
    let krate = crate_with(&[
        ("lib.rs", "pub mod jobs;"),
        (
            "jobs.rs",
            r#"#[cron("1 * * * * *")] pub async fn tick() {}"#,
        ),
    ]);
    fs::create_dir_all(krate.path().join("examples")).unwrap();
    let demo = krate.path().join("examples/demo.rs");
    fs::write(
        &demo,
        r#"#[cron("2 * * * * *")] pub async fn demo_job() {}"#,
    )
    .unwrap();
    let src = krate.path().join("src");
    let storage = [
        stored("tick", Some(src.join("jobs.rs").display().to_string())),
        stored("demo_job", Some(demo.display().to_string())),
    ];
    let jobs = {
        let _ide = crate::macro_storage::test_ide::force();
        discover(&storage, Some(&src), Some(&src.join("lib.rs")))
    };
    assert_eq!(summary(&jobs), rows(&[("jobs", "tick", "0 0 0 * * *")]));
    let error = discover_cron_jobs(&storage, Some(&src), Some(&src.join("lib.rs"))).unwrap_err();
    assert!(
        error.contains("cannot place `#[cron]` job `demo_job`"),
        "{error}"
    );
}
