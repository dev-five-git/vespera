//! Thread-local cached daemon attachment to the JVM.
//!
//! Every JNI callback into the JVM needs a [`jni::Env`] valid for the
//! calling OS thread.  Non-JVM threads (Tokio workers, `spawn_blocking`
//! pool threads) are not attached, so each callback would otherwise
//! `AttachCurrentThread` + detach — paying that cost **per call**.  On
//! the streaming hot path that is once per body chunk (≈ 4096 times for
//! a 1 GiB / 256 KiB stream), and for async completion once per
//! dispatch.
//!
//! [`with_cached_daemon_env`] resolves the current thread's `JNIEnv`
//! **once** and caches it in thread-local storage; every subsequent
//! call on the same thread reuses it:
//!
//! * If the thread is **already attached** (e.g. a JVM-owned servlet
//!   request thread driving `Runtime::block_on`), its env is *borrowed*
//!   — never detached, because the JVM owns that attachment.
//! * Otherwise the thread is attached as a **daemon**
//!   (`AttachCurrentThreadAsDaemon`, so it never blocks JVM shutdown)
//!   and the attachment is *owned*: it is released with
//!   `DetachCurrentThread` from the thread-local destructor when the OS
//!   thread exits (e.g. a `spawn_blocking` worker reaped after its idle
//!   timeout).  Threads that outlive the process — the leaked static
//!   runtime's workers — simply never run the destructor, which is
//!   harmless at process teardown.
//!
//! # Safety invariant
//!
//! The cached `*mut jni::sys::JNIEnv` is valid **only for the exact
//! `JavaVM` and OS thread that produced it**.  This is upheld structurally:
//!
//! * the pointer lives in a `thread_local!` cell, so it is never
//!   observable from another thread;
//! * the raw `JavaVM` pointer is stored beside it and compared on every
//!   lookup, so an embedding that invokes this bridge with another VM on
//!   the same native thread ejects the stale cache before reuse;
//! * it is produced by `GetEnv` / `AttachCurrentThreadAsDaemon` *for
//!   the current thread* and only ever dereferenced inside the same
//!   [`with_cached_daemon_env`] call that read it back from TLS;
//! * `jni::Env` is `!Send`/`!Sync`, and the borrow handed to the
//!   callback never escapes the closure;
//! * the owning [`CachedEnv`] stays in TLS for the thread's lifetime,
//!   so the env stays attached for as long as the cached pointer is
//!   reachable.
//!
//! A future polled across `.await` points may resume on a different
//! worker thread; that thread simply finds an empty TLS cell and
//! resolves its own env, so correctness does not depend on thread
//! affinity — only the amortised attach count does.

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::ptr;

use jni::errors::jni_error_code_to_result;

/// One thread's cached JVM attachment.  Dropped from the thread-local
/// destructor on thread exit; detaches the JVM only for attachments
/// this module created (`owned`).
struct CachedEnv {
    env_ptr: *mut jni::sys::JNIEnv,
    vm_ptr: *mut jni::sys::JavaVM,
    jvm: jni::JavaVM,
    owned: bool,
}

impl Drop for CachedEnv {
    fn drop(&mut self) {
        if !self.owned {
            // Borrowed a JVM-owned thread's env — the JVM owns the
            // attachment lifecycle, we must not detach it.
            return;
        }
        let raw_vm = self.jvm.get_raw();
        // SAFETY: `raw_vm` is a valid JavaVM pointer for this process.
        // `DetachCurrentThread` runs on the exact OS thread whose daemon
        // attachment we created in `resolve_current_env`, releasing the
        // JVM's per-thread state as that thread exits.
        unsafe {
            ((*(*raw_vm)).v1_1.DetachCurrentThread)(raw_vm);
        }
    }
}

thread_local! {
    /// Cached attachment for the current OS thread (empty until the
    /// first [`with_cached_daemon_env`] call resolves it).
    static DAEMON_ENV: RefCell<Option<CachedEnv>> = const { RefCell::new(None) };
}

/// Attach the current OS thread to the JVM as a daemon and return its
/// `JNIEnv`.
fn attach_daemon_thread(jvm: &jni::JavaVM) -> jni::errors::Result<*mut jni::sys::JNIEnv> {
    let raw_vm = jvm.get_raw();
    let mut env_ptr = ptr::null_mut::<c_void>();
    let mut args = jni::sys::JavaVMAttachArgs {
        version: jni::JNIVersion::V1_4.into(),
        name: ptr::null_mut(),
        group: ptr::null_mut(),
    };

    // SAFETY: `raw_vm` comes from `Env::get_java_vm()` and is therefore a
    // valid JavaVM pointer for this process.  JNI 1.4 provides
    // `AttachCurrentThreadAsDaemon`; the returned `JNIEnv` is valid only
    // on the current OS thread and is cached in thread-local storage by
    // the sole caller below.
    let res = unsafe {
        ((*(*raw_vm)).v1_4.AttachCurrentThreadAsDaemon)(
            raw_vm,
            &raw mut env_ptr,
            (&raw mut args).cast::<c_void>(),
        )
    };
    jni_error_code_to_result(res)?;
    if env_ptr.is_null() {
        return Err(jni::errors::Error::NullPtr("AttachCurrentThreadAsDaemon"));
    }

    Ok(env_ptr.cast())
}

/// Resolve the current thread's `JNIEnv`, returning `(env, owned)`.
///
/// `owned == false` when the thread was **already** attached (the JVM
/// owns it — do not detach); `owned == true` when this call attached it
/// as a daemon (we detach on thread exit).
fn resolve_current_env(jvm: &jni::JavaVM) -> jni::errors::Result<(*mut jni::sys::JNIEnv, bool)> {
    let raw_vm = jvm.get_raw();
    let mut env_ptr = ptr::null_mut::<c_void>();
    let version: jni::sys::jint = jni::JNIVersion::V1_4.into();

    // SAFETY: `raw_vm` is a valid JavaVM pointer.  `GetEnv` reports
    // whether the current thread is already attached without creating a
    // new attachment.
    let res = unsafe { ((*(*raw_vm)).v1_2.GetEnv)(raw_vm, &raw mut env_ptr, version) };
    if res == jni::sys::JNI_OK && !env_ptr.is_null() {
        // Already attached (e.g. a JVM-owned request thread) — borrow it.
        return Ok((env_ptr.cast(), false));
    }

    // Not attached (Tokio worker / spawn_blocking thread): attach as a
    // daemon and take ownership of the attachment lifecycle.
    let env_ptr = attach_daemon_thread(jvm)?;
    Ok((env_ptr, true))
}

/// Run `callback` with a [`jni::Env`] for the current thread, resolving
/// (and caching) the attachment on first use and reusing it thereafter.
///
/// The callback runs inside a fresh local-reference frame (so JNI local
/// refs created per call do not accumulate on the long-lived thread),
/// and any pending JVM exception is cleared afterwards — replacing the
/// scoped-detach cleanup that jni-rs runs for transient attachments but
/// cached attachments intentionally skip.
///
/// Panics from `callback` are caught, the exception state is scrubbed,
/// and the panic is resumed so unwinding still cannot cross the FFI
/// boundary uncaught at the JNI entry point.
pub fn with_cached_daemon_env<F, T, E>(jvm: &jni::JavaVM, callback: F) -> std::result::Result<T, E>
where
    F: FnOnce(&mut jni::Env<'_>) -> std::result::Result<T, E>,
    E: From<jni::errors::Error>,
{
    with_cached_daemon_env_impl(jvm, true, callback)
}

/// Like [`with_cached_daemon_env`] but **without** wrapping `callback` in
/// a JNI local-reference frame.
///
/// For the streaming chunk callbacks (`make_pull_closure` /
/// `make_push_closure`) whose hot path uses cached-`JMethodID`
/// `call_method_unchecked` + `get_region`/`set_region` and therefore
/// creates **no** JNI local references per chunk — so the per-chunk
/// `PushLocalFrame`/`PopLocalFrame` of [`with_cached_daemon_env`] is pure
/// overhead (≈ 4096 frame pairs for a 1 GiB / 256 KiB stream).  The
/// pending-exception scrub and panic handling are preserved identically;
/// only the local frame is dropped.
///
/// Callbacks that DO create local refs (e.g. `byte_array_from_slice` in
/// `complete_future` / `call_header_consumer`) MUST keep using
/// [`with_cached_daemon_env`] so those refs are reclaimed per call.
pub fn with_cached_daemon_env_no_frame<F, T, E>(
    jvm: &jni::JavaVM,
    callback: F,
) -> std::result::Result<T, E>
where
    F: FnOnce(&mut jni::Env<'_>) -> std::result::Result<T, E>,
    E: From<jni::errors::Error>,
{
    with_cached_daemon_env_impl(jvm, false, callback)
}

/// Shared implementation of [`with_cached_daemon_env`] (frame) and
/// [`with_cached_daemon_env_no_frame`] (no frame).
fn with_cached_daemon_env_impl<F, T, E>(
    jvm: &jni::JavaVM,
    use_local_frame: bool,
    callback: F,
) -> std::result::Result<T, E>
where
    F: FnOnce(&mut jni::Env<'_>) -> std::result::Result<T, E>,
    E: From<jni::errors::Error>,
{
    DAEMON_ENV.with(|cell| {
        // Resolve + cache under a short-lived borrow, then release it
        // before running the callback so a nested call on the same thread
        // cannot double-borrow the cell.
        let env_ptr = {
            let mut slot = cell.borrow_mut();
            let requested_vm = jvm.get_raw();
            if slot
                .as_ref()
                .is_some_and(|cached| cached.vm_ptr != requested_vm)
            {
                *slot = None;
            }
            if slot.is_none() {
                let (env_ptr, owned) = resolve_current_env(jvm)?;
                *slot = Some(CachedEnv {
                    env_ptr,
                    vm_ptr: requested_vm,
                    jvm: jvm.clone(),
                    owned,
                });
            }
            slot.as_ref()
                .map(|cached| cached.env_ptr)
                .expect("cache populated above")
        };

        // SAFETY: `env_ptr` was resolved for this exact OS thread (see
        // the module-level safety invariant) and is confined to this
        // thread's TLS cell; it is never shared across threads.  The
        // owning `CachedEnv` remains in TLS, so the attachment outlives
        // this borrow.  When `use_local_frame` is true a per-call local
        // frame prevents local-ref accumulation on the long-lived thread;
        // the no-frame path is reserved for callbacks that create none.
        let mut guard = unsafe { jni::AttachGuard::from_unowned(env_ptr) };
        let env = guard.borrow_env_mut();
        let result = catch_unwind(AssertUnwindSafe(|| {
            if use_local_frame {
                env.with_local_frame(jni::DEFAULT_LOCAL_FRAME_CAPACITY, callback)
            } else {
                callback(env)
            }
        }));

        if env.exception_check() {
            env.exception_clear();
        }

        match result {
            Ok(callback_result) => callback_result,
            Err(payload) => resume_unwind(payload),
        }
    })
}
