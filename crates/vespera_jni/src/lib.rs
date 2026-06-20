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
/// `_` `-`).  Each `factory` is an expression evaluating to a
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
        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: $crate::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> $crate::jni::sys::jint {
            // Each user factory runs here (router construction); a panic
            // must never unwind across this `extern "system"` boundary
            // into the JVM.  Catch it and fail library load with
            // `JNI_ERR` instead of aborting the host process.
            let loaded = ::std::panic::catch_unwind(|| {
                $(
                    $crate::vespera_inprocess::register_app_named($name, $factory);
                )+
            });
            match loaded {
                ::std::result::Result::Ok(()) => $crate::jni::sys::JNI_VERSION_1_8,
                ::std::result::Result::Err(_) => $crate::jni::sys::JNI_ERR,
            }
        }
    };
}

// Everything below requires a JVM — excluded from coverage.
#[cfg(not(tarpaulin_include))]
// SAFETY SCOPE: daemon attach/detach uses raw JNI invocation table calls.
#[allow(unsafe_code)]
mod daemon_env;
#[cfg(not(tarpaulin_include))]
// SAFETY SCOPE: byte-array transfers write directly into uninitialized Vec capacity.
#[allow(unsafe_code)]
mod jni_buf;
#[cfg(not(tarpaulin_include))]
// SAFETY SCOPE: JNI exports and direct-buffer submodule contain FFI entry points.
#[allow(unsafe_code)]
mod jni_impl;
#[cfg(not(tarpaulin_include))]
// SAFETY SCOPE: streaming callbacks use cached JMethodID calls and signed-byte views.
#[allow(unsafe_code)]
mod streaming_closures;
