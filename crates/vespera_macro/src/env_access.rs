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

use std::env::VarError;
use std::ffi::OsString;

/// [`std::env::var_os`], honouring the per-thread test overrides.
pub fn var_os(key: &str) -> Option<OsString> {
    if let Some(value) = test_env::lookup(key) {
        return value.into_os_string();
    }
    std::env::var_os(key)
}

/// [`std::env::var`], honouring the per-thread test overrides.
pub fn var(key: &str) -> Result<String, VarError> {
    var_os(key).map_or(Err(VarError::NotPresent), |value| {
        value.into_string().map_err(VarError::NotUnicode)
    })
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
