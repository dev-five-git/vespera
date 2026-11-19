//! Isolated child-process runs for unit tests that pin process-global state
//! (`OnceLock` setters, the shared runtime's shutdown).
//!
//! The parent re-runs its own test binary with `--exact <test>` and a marker
//! env var set; the child then runs the real assertions.  libtest exits 0
//! when a filter matches nothing, so the parent also insists that the child
//! reports `1 passed`: a renamed or moved test then fails loudly instead of
//! silently skipping its assertions.

use std::process::Command;

/// Re-run `test` (its full `module::path::name`) alone in a child process,
/// with `marker` set, `set` env vars applied and `removed` env vars cleared.
/// Panics unless the child exits successfully and ran exactly that one test.
pub fn run_isolated(test: &str, marker: &str, set: &[(&str, &str)], removed: &[&str]) {
    let mut cmd = Command::new(std::env::current_exe().expect("current unit-test executable"));
    cmd.args(["--exact", test, "--nocapture"]).env(marker, "1");
    for (key, value) in set {
        cmd.env(key, value);
    }
    for key in removed {
        cmd.env_remove(key);
    }
    let output = cmd.output().expect("run isolated child test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated test `{test}` did not run its one test:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
mod tests {
    use super::run_isolated;

    /// A name that matches no test used to pass vacuously (exit 0, `0 passed`).
    #[test]
    #[cfg_attr(miri, ignore = "re-executes the test binary as a child process")]
    #[should_panic(expected = "did not run its one test")]
    fn a_child_that_runs_no_test_fails_the_parent() {
        run_isolated(
            "test_child::tests::no_such_test_anywhere",
            "VESPERA_TEST_CHILD_MISSING_MARKER",
            &[],
            &[],
        );
    }
}
