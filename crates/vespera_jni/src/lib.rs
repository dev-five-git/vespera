//! JNI bridge for vespera.
//!
//! # Quick start
//!
//! ```ignore
//! vespera_jni::jni_app!(create_app);
//! ```
//!
//! The [`jni_app!`] macro generates `JNI_OnLoad` which registers your
//! router factory via [`vespera_inprocess::register_app`].  The JNI
//! dispatch symbol is exported by this crate, matching the fixed Java
//! class `com.devfive.vespera.bridge.VesperaBridge`.

#![allow(unsafe_code)]

use std::sync::LazyLock;

pub use vespera_inprocess;

/// Re-export `jni` so the `jni_app!` macro resolves without a direct dep.
pub use jni;

// ── Global Tokio Runtime ─────────────────────────────────────────────

/// Multi-threaded Tokio runtime shared across all JNI calls.
pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime")
});

/// Generate the `JNI_OnLoad` export that registers your app.
///
/// ```ignore
/// vespera_jni::jni_app!(create_app);
/// ```
#[macro_export]
macro_rules! jni_app {
    ($factory:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn JNI_OnLoad(
            _vm: $crate::jni::JavaVM,
            _: *mut ::std::ffi::c_void,
        ) -> $crate::jni::sys::jint {
            $crate::vespera_inprocess::register_app($factory);
            $crate::jni::sys::JNI_VERSION_1_8
        }
    };
}

// ── JNI Export (JVM-only) ────────────────────────────────────────────

#[cfg(not(tarpaulin_include))]
mod jni_export {
    use jni::JNIEnv;
    use jni::objects::{JClass, JString};
    use jni::sys::jstring;

    use super::RUNTIME;

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatch(String) -> String`
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatch<'local>(
        mut env: JNIEnv<'local>,
        _class: JClass<'local>,
        request_json: JString<'local>,
    ) -> jstring {
        let input = if let Ok(s) = env.get_string(&request_json) {
            String::from(s)
        } else {
            let err = vespera_inprocess::serialize_error("invalid request envelope string");
            return env
                .new_string(err)
                .map(JString::into_raw)
                .unwrap_or(std::ptr::null_mut());
        };

        let json = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vespera_inprocess::dispatch_from_json(&input, &RUNTIME)
        }))
        .unwrap_or_else(|_| vespera_inprocess::serialize_error("panic in Rust engine"));

        env.new_string(json)
            .map(JString::into_raw)
            .unwrap_or(std::ptr::null_mut())
    }
}
