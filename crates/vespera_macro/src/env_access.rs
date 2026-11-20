//! The macro's only window onto the process environment.
//!
//! Every environment read in this crate (`CARGO_MANIFEST_DIR`,
//! `CARGO_PKG_NAME`, `VESPERA_*`, ...) goes through [`var`] / [`var_os`]. In
//! production they are plain `std::env` reads. Under `cfg(test)` a
//! **thread-local** override layer sits in front of the real environment, so
//! a unit test can point `CARGO_MANIFEST_DIR` at its own tempdir without
//! touching the process environment that every concurrently running test
//! reads. Mutating the process environment from a test (`std::env::set_var`,
//! even under `#[serial]`) raced every *non*-serial reader — the cause of the
//! intermittent `process_export_app` / `openapi_io` sidecar failures.
//!
//! Unit tests are also hermetic for the variables that redirect where the
//! macro writes or what it documents (`CARGO_TARGET_DIR`, `VESPERA_*`): a
//! developer's real values never reach a test that does not override them,
//! so a shared absolute target directory cannot route a tempdir crate's
//! sidecars into the real `<target>/vespera`.

use std::borrow::Cow;
use std::env::VarError;
use std::ffi::OsString;

/// [`std::env::var_os`], honouring the per-thread test overrides.
pub fn var_os(key: &str) -> Option<OsString> {
    if let Some(value) = test_env::lookup(key) {
        return value.into_os_string();
    }
    if hidden_from_tests(key) {
        return None;
    }
    std::env::var_os(key)
}

/// Whether the real value of `key` is hidden from unit tests (always `false`
/// outside `cfg(test)`): `CARGO_TARGET_DIR` and every `VESPERA_*` variable,
/// which a developer may export for real builds. A test that needs one sets
/// it with [`test_env::set_var`].
fn hidden_from_tests(key: &str) -> bool {
    cfg!(test) && (key == "CARGO_TARGET_DIR" || key.starts_with("VESPERA_"))
}

/// [`std::env::var`], honouring the per-thread test overrides.
pub fn var(key: &str) -> Result<String, VarError> {
    var_os(key).map_or(Err(VarError::NotPresent), |value| {
        value.into_string().map_err(VarError::NotUnicode)
    })
}

/// The value of an optional setting read by [`var`], where a set-but-empty
/// variable (`VESPERA_DIR=`) counts as unset, as the README documents for
/// every `VESPERA_*` fallback.
pub fn non_empty_var(key: &str) -> Option<String> {
    var(key).ok().filter(|value| !value.is_empty())
}

/// `err`, naming the environment variable its value was read from: the error
/// points at the macro call, so the variable name is the only pointer to it.
pub fn env_error(err: &syn::Error, key: &str) -> syn::Error {
    syn::Error::new(
        err.span(),
        format!("{err} (from the `{key}` environment variable)"),
    )
}

/// Test-override key for [`compiler_args`] (newline-separated arguments);
/// not a valid environment variable name, so it never shadows a real one.
pub const COMPILER_ARGS_KEY: &str = "\u{0}vespera:compiler-args";

/// The command line of the process the macro expands in, `@argfile`s
/// expanded (cargo moves long rustc command lines into one).
///
/// A proc macro runs inside the compiler (`rustc`, `rustdoc`,
/// `clippy-driver`), so this is the compiler's own invocation — the only
/// place a macro can observe what cargo is building: `--test` for a test
/// harness build (`cfg(test)` is otherwise invisible to proc macros) and
/// `--out-dir <target>/<profile>/deps` for the real target directory.  In a
/// proc-macro server (rust-analyzer) it is the server's command line.
/// Honours the per-thread test override [`COMPILER_ARGS_KEY`].
///
/// Borrowed from a process-wide memo outside tests: the argv (hundreds of
/// `--extern` / `-L` arguments) is read on hot paths such as every cfg
/// evaluation of a source lookup, so it is never copied per call.
pub fn compiler_args() -> Cow<'static, [String]> {
    static ARGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    if let Some(value) = test_env::lookup(COMPILER_ARGS_KEY) {
        return Cow::Owned(
            value
                .into_os_string()
                .map(|args| {
                    args.to_string_lossy()
                        .split('\n')
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        );
    }
    Cow::Borrowed(ARGS.get_or_init(|| {
        expand_argfiles(std::env::args_os().map(|arg| arg.to_string_lossy().into_owned()))
    }))
}

/// Whether [`compiler_args`] currently reads a per-thread test override (so
/// a memo derived from it must not be used).
pub fn compiler_args_overridden() -> bool {
    test_env::lookup(COMPILER_ARGS_KEY).is_some()
}

/// Replace every readable `@path` argument by the lines of that file (the
/// rustc argfile format: one argument per line).
fn expand_argfiles(args: impl Iterator<Item = String>) -> Vec<String> {
    args.flat_map(|arg| {
        arg.strip_prefix('@')
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(
                || vec![arg.clone()],
                |content| content.lines().map(str::to_owned).collect(),
            )
    })
    .collect()
}

/// Per-thread environment overrides for unit tests.
///
/// Each libtest test runs on its own thread, so an override is invisible to
/// every other test. [`EnvGuard`] restores the previous override on drop
/// (including on panic), so nested / sequential overrides compose.
// Compiled in every build so callers (`var_os`, the parallel path worker
// pool) need no statement-level `#[cfg(test)]`; outside tests nothing ever
// sets an override, so the map stays empty and lookups miss.
pub mod test_env {
    use std::cell::RefCell;
    use std::collections::HashMap;
    #[cfg(test)]
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::sync::Arc;

    /// One overridden variable.
    #[derive(Clone)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub enum Override {
        /// The variable reads as this value.
        Set(OsString),
        /// The variable reads as absent.
        Unset,
    }

    impl Override {
        pub(super) fn into_os_string(self) -> Option<OsString> {
            match self {
                Self::Set(value) => Some(value),
                Self::Unset => None,
            }
        }
    }

    /// This thread's overrides, by variable name.
    pub type Overrides = Arc<HashMap<String, Override>>;

    thread_local! {
        static OVERRIDES: RefCell<Overrides> = RefCell::new(Arc::default());
    }

    pub(super) fn lookup(key: &str) -> Option<Override> {
        OVERRIDES.with(|overrides| overrides.borrow().get(key).cloned())
    }

    /// This thread's overrides, for handing to worker threads ([`install`]).
    pub fn snapshot() -> Overrides {
        OVERRIDES.with(|overrides| Arc::clone(&overrides.borrow()))
    }

    /// Install `overrides` on this (worker) thread.
    pub fn install(overrides: Overrides) {
        OVERRIDES.with(|slot| *slot.borrow_mut() = overrides);
    }

    #[cfg(test)]
    fn replace(key: &str, value: Override) -> EnvGuard {
        let previous = OVERRIDES.with(|overrides| {
            let mut overrides = overrides.borrow_mut();
            Arc::make_mut(&mut overrides).insert(key.to_owned(), value)
        });
        EnvGuard {
            key: key.to_owned(),
            previous,
        }
    }

    #[cfg(test)]
    /// Override `key` with `value` on this thread until the guard drops.
    #[must_use = "the override is removed as soon as the guard drops"]
    pub fn set_var(key: &str, value: impl AsRef<OsStr>) -> EnvGuard {
        replace(key, Override::Set(value.as_ref().to_owned()))
    }

    #[cfg(test)]
    /// Hide `key` on this thread until the guard drops.
    #[must_use = "the override is removed as soon as the guard drops"]
    pub fn remove_var(key: &str) -> EnvGuard {
        replace(key, Override::Unset)
    }

    #[cfg(test)]
    /// Restores the override `key` had before [`set_var`] / [`remove_var`].
    #[must_use = "the override is removed as soon as the guard drops"]
    pub struct EnvGuard {
        key: String,
        previous: Option<Override>,
    }

    #[cfg(test)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            let previous = self.previous.take();
            OVERRIDES.with(|overrides| {
                let mut overrides = overrides.borrow_mut();
                let overrides = Arc::make_mut(&mut overrides);
                match previous {
                    Some(value) => overrides.insert(self.key.clone(), value),
                    None => overrides.remove(&self.key),
                };
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::{install, remove_var, set_var, snapshot};
    use super::*;

    const KEY: &str = "__VESPERA_ENV_ACCESS_TEST__";

    #[test]
    fn overrides_are_scoped_to_the_guard_and_nest() {
        assert_eq!(var(KEY), Err(VarError::NotPresent));
        {
            let _outer = set_var(KEY, "outer");
            assert_eq!(var(KEY).as_deref(), Ok("outer"));
            {
                let _hidden = remove_var(KEY);
                assert_eq!(var_os(KEY), None);
            }
            assert_eq!(var(KEY).as_deref(), Ok("outer"));
        }
        assert_eq!(var(KEY), Err(VarError::NotPresent));
    }

    #[test]
    fn real_environment_is_read_without_an_override() {
        // cargo sets CARGO_PKG_NAME for test binaries.
        assert_eq!(var("CARGO_PKG_NAME").as_deref(), Ok("vespera_macro"));
    }

    #[test]
    fn overrides_are_invisible_to_other_threads_until_installed() {
        let _guard = set_var(KEY, "mine");
        let overrides = snapshot();
        std::thread::spawn(|| assert_eq!(var_os(KEY), None))
            .join()
            .unwrap();
        std::thread::spawn(move || {
            install(overrides);
            assert_eq!(var(KEY).as_deref(), Ok("mine"));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn compiler_args_expand_argfiles_and_honour_the_override() {
        let temp = tempfile::TempDir::new().unwrap();
        let argfile = temp.path().join("rustc.args");
        std::fs::write(&argfile, "--crate-name\ndemo\n--test\n").unwrap();
        let args = [
            "rustc".to_owned(),
            format!("@{}", argfile.display()),
            "@missing-argfile".to_owned(),
        ];
        assert_eq!(
            expand_argfiles(args.into_iter()),
            [
                "rustc",
                "--crate-name",
                "demo",
                "--test",
                "@missing-argfile"
            ]
        );
        // The test binary's own command line (no rustc flags).
        assert!(!compiler_args().iter().any(|arg| arg == "--test"));
        {
            let _args = set_var(COMPILER_ARGS_KEY, "rustc\n--test");
            assert_eq!(*compiler_args(), ["rustc", "--test"]);
        }
        let _args = remove_var(COMPILER_ARGS_KEY);
        assert!(compiler_args().is_empty());
    }

    /// A developer's exported `CARGO_TARGET_DIR` / `VESPERA_*` never reach a
    /// unit test: re-runs this test in a child process with both set (the
    /// parent's environment cannot be mutated safely under parallel tests).
    #[test]
    fn real_target_dir_and_vespera_variables_are_hidden_from_tests() {
        const CHILD: &str = "__VESPERA_HERMETIC_ENV_CHILD__";
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(var_os("CARGO_TARGET_DIR"), None);
            assert_eq!(var_os("VESPERA_TARGET_DIR"), None);
            assert_eq!(var_os("VESPERA_TITLE"), None);
            assert!(var_os(CHILD).is_some());
            let temp = tempfile::TempDir::new().unwrap();
            let target = crate::vespera_impl::find_target_dir(temp.path());
            assert!(target.starts_with(temp.path()), "{}", target.display());
            let _title = set_var("VESPERA_TITLE", "set by the test");
            assert_eq!(var("VESPERA_TITLE").as_deref(), Ok("set by the test"));
            return;
        }
        let real_target = std::env::temp_dir().join("vespera-shared-target");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "env_access::tests::real_target_dir_and_vespera_variables_are_hidden_from_tests",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("CARGO_TARGET_DIR", &real_target)
            .env("VESPERA_TARGET_DIR", &real_target)
            .env("VESPERA_TITLE", "exported by the developer")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[cfg(unix)]
    fn non_unicode() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![0x66, 0x80])
    }

    #[cfg(windows)]
    fn non_unicode() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0x66, 0xD800])
    }

    #[test]
    fn non_unicode_value_is_reported_like_std() {
        let _guard = set_var(KEY, non_unicode());
        assert_eq!(var(KEY), Err(VarError::NotUnicode(non_unicode())));
    }
}
