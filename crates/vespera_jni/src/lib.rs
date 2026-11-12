//! JNI bridge for vespera.
//!
//! # Quick start
//!
//! ```ignore
//! vespera::jni_app!(create_app);
//! ```
//!
//! The [`jni_app!`] macro generates `JNI_OnLoad` which registers your
//! router factory via [`vespera_inprocess::register_app`].  The JNI
//! dispatch symbol is exported by this crate, matching the fixed Java
//! class `com.devfive.vespera.bridge.VesperaBridge`.

#![cfg(not(tarpaulin_include))]

pub use jni;
pub use vespera_inprocess;

/// mimalloc as the process-wide allocator (feature `mimalloc`).
///
/// The JNI dispatch hot path allocates several times per call (input
/// buffer, request body, response collection, wire response); the OS
/// default allocator — Windows `HeapAlloc` in particular — is
/// measurably slower than mimalloc on this pattern.  Opt-in because a
/// `#[global_allocator]` is process-wide and belongs to the final
/// cdylib's build decision.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL_ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Generate the `JNI_OnLoad` export that registers a single (default)
/// app.  Backward-compatible sugar for the single-app case; new code
/// targeting multiple apps should use [`jni_apps!`] directly.
///
/// ```ignore
/// vespera::jni_app!(create_app);
/// ```
///
/// Expands to `register_app(factory)` inside the generated
/// `JNI_OnLoad`.  The resulting router is reachable from Java
/// without an `X-Vespera-App` header (or with the header set to
/// `"_default"`).
// SAFETY SCOPE: this macro intentionally emits `#[unsafe(no_mangle)]` for the
// single required `JNI_OnLoad` export; keep the unsafe allowance local so other
// crate-root code still trips the workspace unsafe lint.
#[allow(unsafe_code)]
#[macro_export]
macro_rules! jni_app {
    ($factory:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: $crate::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> $crate::jni::sys::jint {
            // The user factory runs here (router construction); a panic
            // must never unwind across this `extern "system"` boundary
            // into the JVM.  Catch it and fail library load with
            // `JNI_ERR` instead of aborting the host process.
            let loaded = ::std::panic::catch_unwind(|| {
                $crate::vespera_inprocess::register_app($factory);
            });
            match loaded {
                ::std::result::Result::Ok(()) => $crate::jni::sys::JNI_VERSION_1_8,
                ::std::result::Result::Err(_) => $crate::jni::sys::JNI_ERR,
            }
        }
    };
}

/// Generate the `JNI_OnLoad` export that registers **multiple named
/// apps** in a single declaration.  This is the primary multi-app
/// entry point — exactly one `JNI_OnLoad` per cdylib is generated,
/// regardless of how many apps you register.
///
/// ```ignore
/// vespera::jni_apps! {
///     "admin"  => admin_app,
///     "public" => public_app,
/// }
/// ```
///
/// Each `name` must be a string literal matching the validation rules
/// in [`register_app_named`] (non-empty, ≤ 64 bytes, alphanumeric +
/// `_` `-`), and names must be unique (compared after trimming).  Both
/// rules are checked **at compile time**: an invalid or duplicated name is
/// a build error instead of an app that is silently never registered.  A
/// registration that still fails at load time (e.g. a factory re-entering
/// the registry) is reported on stderr and fails `System.loadLibrary` with
/// `JNI_ERR`.  Each `factory` is an expression evaluating to a
/// `Fn() -> Router + Send + Sync + 'static` (typically a `pub fn`
/// path).
///
/// From the Java side, the request's `X-Vespera-App` header
/// (configurable) selects which app receives the dispatch.  Requests
/// without the header are routed to the default app (registered via
/// [`jni_app!`] or `register_app`); requests naming an unregistered
/// app receive a 404 wire response.
///
/// # Composition
///
/// JNI requires exactly one `JNI_OnLoad` per cdylib.  Use `jni_apps!`
/// (or `jni_app!`) **once** in the cdylib's root module; assemble
/// factories from submodules into that single invocation.  Using
/// `jni_app!` and `jni_apps!` together — or `jni_apps!` more than
/// once — will produce a duplicate-symbol link error.
///
/// [`register_app_named`]: vespera_inprocess::register_app_named
// SAFETY SCOPE: this macro intentionally emits `#[unsafe(no_mangle)]` for the
// single required `JNI_OnLoad` export; keep the unsafe allowance local so other
// crate-root code still trips the workspace unsafe lint.
#[allow(unsafe_code)]
#[macro_export]
macro_rules! jni_apps {
    ( $( $name:literal => $factory:expr ),+ $(,)? ) => {
        const _: () = {
            ::std::assert!(
                $crate::__jni_app_names_valid(&[$($name),+]),
                "jni_apps!: every app name must be non-empty, at most 64 bytes, and only contain [A-Za-z0-9_-]"
            );
            ::std::assert!(
                $crate::__jni_app_names_unique(&[$($name),+]),
                "jni_apps!: duplicate app name — each app may be registered only once"
            );
        };

        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: $crate::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> $crate::jni::sys::jint {
            // Each user factory runs here (router construction); a panic
            // must never unwind across this `extern "system"` boundary
            // into the JVM.  Catch it and fail library load with
            // `JNI_ERR` instead of aborting the host process.
            //
            // `Ok(false)` (name already registered) is accepted: it only
            // happens when the library is loaded again into a process that
            // already registered these apps, where first-wins is intended.
            let loaded = ::std::panic::catch_unwind(|| -> bool {
                $(
                    if let ::std::result::Result::Err(err) =
                        $crate::vespera_inprocess::try_register_app_named($name, $factory)
                    {
                        ::std::eprintln!(
                            "vespera: jni_apps! failed to register app {:?}: {}",
                            $name,
                            err
                        );
                        return false;
                    }
                )+
                true
            });
            match loaded {
                ::std::result::Result::Ok(true) => $crate::jni::sys::JNI_VERSION_1_8,
                _ => $crate::jni::sys::JNI_ERR,
            }
        }
    };
}

/// Compile-time check behind [`jni_apps!`]: every name satisfies the
/// [`vespera_inprocess::register_app_named`] rules (trimmed, non-empty,
/// ≤ 64 bytes, `[A-Za-z0-9_-]`).
#[doc(hidden)]
#[must_use]
pub const fn __jni_app_names_valid(names: &[&str]) -> bool {
    let mut i = 0;
    while i < names.len() {
        let name = names[i].as_bytes().trim_ascii();
        if name.is_empty() || name.len() > 64 {
            return false;
        }
        let mut j = 0;
        while j < name.len() {
            let b = name[j];
            if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// Compile-time check behind [`jni_apps!`]: no two names are equal after
/// trimming (the registry trims before keying).
#[doc(hidden)]
#[must_use]
pub const fn __jni_app_names_unique(names: &[&str]) -> bool {
    let mut i = 0;
    while i < names.len() {
        let mut j = i + 1;
        while j < names.len() {
            if const_bytes_eq(
                names[i].as_bytes().trim_ascii(),
                names[j].as_bytes().trim_ascii(),
            ) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

const fn const_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Everything below requires a JVM — excluded from coverage.
// SAFETY SCOPE: daemon attach/detach uses raw JNI invocation table calls.
#[allow(unsafe_code)]
mod daemon_env;
// SAFETY SCOPE: byte-array transfers write directly into uninitialized Vec capacity.
#[allow(unsafe_code)]
mod jni_buf;
// SAFETY SCOPE: JNI exports and direct-buffer submodule contain FFI entry points.
#[allow(unsafe_code)]
pub(crate) mod jni_impl;

/// Process-wide JNI runtime / admission knobs.  Like the
/// `configure*0` JNI hooks (which call them), they are first-wins and must
/// run before the first dispatch; each returns `false` once fixed.
pub use jni_impl::{
    DEFAULT_ASYNC_MAX_IN_FLIGHT, DEFAULT_RUNTIME_MAX_BLOCKING_THREADS, async_max_in_flight,
    runtime_max_blocking_threads, runtime_worker_threads, set_async_max_in_flight,
    set_runtime_max_blocking_threads, set_runtime_worker_threads,
};
// SAFETY SCOPE: streaming-with-header JNI exports contain FFI entry points.
#[allow(unsafe_code)]
pub(crate) mod jni_impl_streaming_header;
// SAFETY SCOPE: streaming callbacks use cached JMethodID calls and signed-byte views.
#[allow(unsafe_code)]
mod streaming_closures;

#[cfg(test)]
mod app_name_check_tests {
    use rstest::rstest;

    #[rstest]
    #[case::single_default(&["_default"], true, true)]
    #[case::several(&["_default", "admin", "public-v2"], true, true)]
    #[case::duplicate(&["admin", "admin"], true, false)]
    #[case::duplicate_after_trim(&["admin", " admin "], true, false)]
    #[case::empty(&["  "], false, true)]
    #[case::bad_char(&["ad min"], false, true)]
    #[case::too_long(&["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"], false, true)]
    fn compile_time_app_name_checks(
        #[case] names: &[&str],
        #[case] valid: bool,
        #[case] unique: bool,
    ) {
        assert_eq!(super::__jni_app_names_valid(names), valid);
        assert_eq!(super::__jni_app_names_unique(names), unique);
    }
}
