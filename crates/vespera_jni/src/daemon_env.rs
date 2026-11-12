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
//! [`with_cached_daemon_env`] asks the JVM for the current thread's
//! `JNIEnv` with `GetEnv` on **every** call (a cheap, non-attaching
//! lookup) and only attaches when the thread is not attached:
//!
//! * If the thread is **already attached** (e.g. a JVM-owned servlet
//!   request thread driving `Runtime::block_on`), its env is *borrowed*
//!   for this one call and **never cached** — the JVM owns that
//!   attachment and may detach and later re-attach the thread, after
//!   which a cached `JNIEnv` pointer would dangle (use-after-detach UB).
//! * Otherwise the thread is attached as a **daemon**
//!   (`AttachCurrentThreadAsDaemon`, so it never blocks JVM shutdown)
//!   and the attachment is *owned*: only that ownership record is kept
//!   in TLS, and it releases the attachment with `DetachCurrentThread`
//!   from the thread-local destructor when the OS thread exits (e.g. a
//!   `spawn_blocking` worker reaped after its idle timeout).  Threads
//!   that outlive the process — the leaked static runtime's workers —
//!   simply never run the destructor, which is harmless at process
//!   teardown.  The attach cost is therefore still paid once per thread
//!   (the expensive part); the per-call `GetEnv` is a pointer lookup.
//!
//! # Safety invariant
//!
//! No `*mut jni::sys::JNIEnv` outlives the call that obtained it: the
//! pointer is produced by `GetEnv` / `AttachCurrentThreadAsDaemon` *for
//! the current thread* inside [`with_cached_daemon_env`] and dereferenced
//! only within that same call, while the thread is provably attached.
//! `jni::Env` is `!Send`/`!Sync`, and the borrow handed to the callback
//! never escapes the closure.  The TLS record only remembers that THIS
//! module owns the thread's attachment (and for which `JavaVM`), so the
//! detach on thread exit targets the right VM; a record for a different VM
//! is replaced (detaching the old one) before attaching to the new one.
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

/// Record of a daemon attachment THIS module created on the current
/// thread.  Dropped from the thread-local destructor on thread exit (or
/// when replaced for a different VM), detaching the thread.  Borrowed
/// (JVM-owned) attachments are never recorded.
struct OwnedAttachment {
    vm_ptr: *mut jni::sys::JavaVM,
    jvm: jni::JavaVM,
}

impl Drop for OwnedAttachment {
    fn drop(&mut self) {
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
    /// Ownership record of this module's daemon attachment of the current
    /// OS thread (empty for unattached or JVM-owned threads).
    static DAEMON_ENV: RefCell<Option<OwnedAttachment>> = const { RefCell::new(None) };
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

/// The current thread's `JNIEnv` if it is attached to `jvm` right now.
fn current_env(jvm: &jni::JavaVM) -> Option<*mut jni::sys::JNIEnv> {
    let raw_vm = jvm.get_raw();
    let mut env_ptr = ptr::null_mut::<c_void>();
    let version: jni::sys::jint = jni::JNIVersion::V1_4.into();

    // SAFETY: `raw_vm` is a valid JavaVM pointer.  `GetEnv` reports
    // whether the current thread is already attached without creating a
    // new attachment.
    let res = unsafe { ((*(*raw_vm)).v1_2.GetEnv)(raw_vm, &raw mut env_ptr, version) };
    (res == jni::sys::JNI_OK && !env_ptr.is_null()).then(|| env_ptr.cast())
}

/// Resolve a `JNIEnv` valid for the current thread for the duration of
/// one call: `GetEnv` first (borrowed or our own still-live attachment),
/// else attach as a daemon and record ownership in `slot`.
fn resolve_current_env(
    jvm: &jni::JavaVM,
    slot: &mut Option<OwnedAttachment>,
) -> jni::errors::Result<*mut jni::sys::JNIEnv> {
    if let Some(env_ptr) = current_env(jvm) {
        return Ok(env_ptr);
    }
    // Not attached (Tokio worker / spawn_blocking thread).  Drop a record
    // for a DIFFERENT VM first (detaching that attachment), then attach as
    // a daemon and take ownership of the attachment lifecycle.  A record
    // for the SAME VM means our own attachment was released behind our
    // back; re-attaching under the existing record is correct.
    let requested_vm = jvm.get_raw();
    if slot
        .as_ref()
        .is_some_and(|owned| owned.vm_ptr != requested_vm)
    {
        *slot = None;
    }
    let env_ptr = attach_daemon_thread(jvm)?;
    if slot.is_none() {
        *slot = Some(OwnedAttachment {
            vm_ptr: requested_vm,
            jvm: jvm.clone(),
        });
    }
    Ok(env_ptr)
}

/// Run `callback` with a [`jni::Env`] for the current thread, attaching it
/// as a daemon on first use (once per thread) and reusing that attachment
/// thereafter; the `JNIEnv` itself is re-resolved with `GetEnv` per call.
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
        // Resolve under a short-lived borrow, then release it before
        // running the callback so a nested call on the same thread cannot
        // double-borrow the cell.
        let env_ptr = resolve_current_env(jvm, &mut cell.borrow_mut())?;

        // SAFETY: `env_ptr` was just resolved by `GetEnv` /
        // `AttachCurrentThreadAsDaemon` for this exact OS thread (see the
        // module-level safety invariant) and is used only within this call,
        // during which nothing on this thread detaches it.  When `use_local_frame` is true a per-call local
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
