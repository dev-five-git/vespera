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

#![allow(unsafe_code)]
#![cfg(not(tarpaulin_include))]

pub use jni;
pub use vespera_inprocess;

/// Generate the `JNI_OnLoad` export that registers your app.
///
/// ```ignore
/// vespera::jni_app!(create_app);
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

// Everything below requires a JVM — excluded from coverage.
#[cfg(not(tarpaulin_include))]
mod jni_impl {
    use std::sync::LazyLock;

    use jni::EnvUnowned;
    use jni::errors::ThrowRuntimeExAndDefault;
    use jni::objects::{JClass, JObject, JString};
    use jni::sys::jstring;

    /// Multi-threaded Tokio runtime shared across all JNI calls.
    pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create Tokio runtime")
    });

    /// `com.devfive.vespera.bridge.VesperaBridge.dispatch(String) -> String`
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatch<'local>(
        mut unowned_env: EnvUnowned<'local>,
        _class: JClass<'local>,
        request_json: JString<'local>,
    ) -> jstring {
        unowned_env
            .with_env(|env| -> jni::errors::Result<JObject<'local>> {
                let Ok(input) = request_json.try_to_string(env) else {
                    let err =
                        vespera_inprocess::serialize_error("invalid request envelope string");
                    return Ok(env.new_string(err)?.into());
                };

                let json = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    vespera_inprocess::dispatch_from_json(&input, &RUNTIME)
                }))
                .unwrap_or_else(|_| vespera_inprocess::serialize_error("panic in Rust engine"));

                Ok(env.new_string(json)?.into())
            })
            .resolve::<ThrowRuntimeExAndDefault>()
            .into_raw()
    }
}
