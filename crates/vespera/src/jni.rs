//! JNI integration for vespera.
//!
//! # Quick start
//!
//! ```ignore
//! // lib.rs — one line is all you need:
//! vespera::jni_app!(create_app);
//! ```
//!
//! The [`jni_app!`](crate::jni_app) macro generates the `JNI_OnLoad`
//! export that registers your router factory.  The JNI dispatch symbol
//! (`Java_com_devfive_vespera_bridge_VesperaBridge_dispatch`) is already
//! exported by this module — matching the fixed Java class
//! `com.devfive.vespera.bridge.VesperaBridge`.

use std::sync::{LazyLock, OnceLock};

use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::jstring,
};

use crate::inprocess;

// ── Global Tokio Runtime ─────────────────────────────────────────────

/// Multi-threaded Tokio runtime shared across all JNI calls.
pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime")
});

// ── App Factory Registration ─────────────────────────────────────────

type AppFactory = Box<dyn Fn() -> axum::Router + Send + Sync>;

static APP_FACTORY: OnceLock<AppFactory> = OnceLock::new();

/// Register the router factory that the JNI dispatch will use.
///
/// Prefer [`jni_app!`](crate::jni_app) instead of calling this
/// directly — the macro generates `JNI_OnLoad` for you.
///
/// # Panics
///
/// Panics if called more than once.
pub fn register_app<F>(factory: F)
where
    F: Fn() -> axum::Router + Send + Sync + 'static,
{
    assert!(
        APP_FACTORY.set(Box::new(factory)).is_ok(),
        "vespera::jni::register_app called more than once"
    );
}

/// Generate the `JNI_OnLoad` export that registers your app.
///
/// # Usage
///
/// ```ignore
/// vespera::jni_app!(create_app);
/// ```
///
/// Expands to:
///
/// ```ignore
/// #[unsafe(no_mangle)]
/// pub extern "system" fn JNI_OnLoad(
///     _vm: jni::JavaVM,
///     _: *mut std::ffi::c_void,
/// ) -> jni::sys::jint {
///     vespera::jni::register_app(create_app);
///     jni::sys::JNI_VERSION_1_8
/// }
/// ```
#[macro_export]
macro_rules! jni_app {
    ($factory:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: ::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> ::jni::sys::jint {
            $crate::jni::register_app($factory);
            ::jni::sys::JNI_VERSION_1_8
        }
    };
}

/// Utility for dispatching from Rust test code.  Delegates to
/// [`inprocess::dispatch`] without going through JNI.
pub async fn dispatch_test(
    create_app: impl FnOnce() -> axum::Router,
    envelope: &inprocess::RequestEnvelope,
) -> String {
    inprocess::dispatch(create_app(), envelope).await
}

// ── JNI Export ────────────────────────────────────────────────────────
//
// Java class: com.devfive.vespera.bridge.VesperaBridge
// Method:     public static native String dispatch(String json)
// Symbol:     Java_com_devfive_vespera_bridge_VesperaBridge_dispatch
// ─────────────────────────────────────────────────────────────────────

/// The single JNI entry point.
///
/// Java calls `VesperaBridge.dispatch(json)` → this function.
/// The router is obtained from the factory registered via
/// [`register_app`].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatch<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    request_json: JString<'local>,
) -> jstring {
    let input = match env.get_string(&request_json) {
        Ok(s) => String::from(s),
        Err(_) => return error_jstring(&mut env, "invalid request envelope string"),
    };

    let json = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Get the registered app factory
        let Some(factory) = APP_FACTORY.get() else {
            return serde_json::to_string(&inprocess::error_envelope(
                "no app registered — call vespera::jni::register_app() in JNI_OnLoad",
            ))
            .unwrap_or_default();
        };

        match inprocess::parse_request(&input) {
            Ok(envelope) => {
                let router = factory();
                RUNTIME.block_on(inprocess::dispatch(router, &envelope))
            }
            Err(msg) => serde_json::to_string(&inprocess::error_envelope(&msg)).unwrap_or_default(),
        }
    }))
    .unwrap_or_else(|_| {
        serde_json::to_string(&inprocess::error_envelope("panic in Rust engine"))
            .unwrap_or_default()
    });

    env.new_string(json)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

fn error_jstring(env: &mut JNIEnv, msg: &str) -> jstring {
    let json = serde_json::to_string(&inprocess::error_envelope(msg)).unwrap_or_default();
    env.new_string(json)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}
