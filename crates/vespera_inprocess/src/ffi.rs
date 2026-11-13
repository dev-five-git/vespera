//! Stable C ABI (feature `ffi`) — embed a vespera app in a non-JVM host
//! (C/C++, Go via cgo, Python `ctypes`, .NET P/Invoke, Node N-API addons)
//! the way `vespera_jni` embeds it in a JVM.
//!
//! The header is hand-written and shipped with the crate:
//! `include/vespera.h` (also available as [`HEADER`]).  The user's cdylib
//! invokes [`ffi_app!`](crate::ffi_app) / [`ffi_apps!`](crate::ffi_apps),
//! which export `vespera_init`; every other symbol is exported from here:
//!
//! | Symbol | Role |
//! |---|---|
//! | `vespera_abi_version()` | [`VESPERA_ABI_VERSION`] the library was built with |
//! | `vespera_init()` | register apps + start the shared runtime (idempotent) |
//! | `vespera_dispatch(req, len, &out, &out_len)` | wire in → Rust-allocated wire out |
//! | `vespera_dispatch_into(req, len, buf, cap, &written)` | wire in → caller buffer; overflow reports the required size |
//! | `vespera_dispatch_async(req, len, cb, user_data)` | wire in → `cb(user_data, out, out_len)` exactly once (`503` over the in-flight cap) |
//! | `vespera_free(ptr, len)` | release a Rust-allocated response |
//! | `vespera_shutdown(timeout_ms)` | drain + stop the shared runtime (irreversible) |
//! | `vespera_configure_*(value)` | process-wide knobs, first-wins (see [`configure`]) |
//!
//! All dispatches run on the same shared runtime as JNI
//! ([`crate::runtime`]), so a cdylib exporting both `jni_app!` and
//! `ffi_app!` serves both hosts from one runtime.  Every entry point catches
//! panics: nothing unwinds across the ABI.

use std::{
    ffi::{c_int, c_void},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    task::Poll,
    time::Duration,
};

use crate::runtime::{
    AsyncPermit, RuntimeLease, async_overloaded_wire, block_on_shared_runtime, runtime,
    runtime_unavailable_wire, shutdown_runtime, try_acquire_async_permit,
};
use crate::{DirectWriteResult, check_ingress_cap, error_wire, panic_error_wire};

// `vespera_configure_*`: the C twins of the JNI `configure*0` hooks.
pub mod configure;
pub use configure::{
    vespera_configure_async_max_in_flight, vespera_configure_dispatch_timeout_ms,
    vespera_configure_max_blocking_threads, vespera_configure_max_buffered_response_bytes,
    vespera_configure_max_request_bytes, vespera_configure_worker_threads,
};

/// The hand-written C header (`include/vespera.h`) matching these symbols.
pub const HEADER: &str = include_str!("../include/vespera.h");

/// Bumped on any incompatible change to the C ABI (`VESPERA_ABI_VERSION`).
pub const VESPERA_ABI_VERSION: u32 = 1;
/// Success (a wire response was produced).
pub const VESPERA_OK: c_int = 0;
/// `vespera_dispatch_into`: the response did not fit; `*written` holds the
/// required size.
pub const VESPERA_OVERFLOW: c_int = 1;
/// A required pointer was NULL, a size was out of range, or buffers overlap.
pub const VESPERA_ERR_INVALID_ARGUMENT: c_int = -1;
/// A Rust panic was caught where no wire response could be returned.
pub const VESPERA_ERR_PANIC: c_int = -2;
/// `vespera_init`: an app could not be registered.
pub const VESPERA_ERR_REGISTRATION: c_int = -3;
/// `vespera_init`: the runtime is unavailable; `vespera_shutdown`: already
/// shut down.
pub const VESPERA_ERR_RUNTIME: c_int = -4;
/// `vespera_configure_*`: the knob was already fixed (an earlier configure
/// call, `vespera_init` starting the runtime, or a dispatch reading it).
pub const VESPERA_ERR_TOO_LATE: c_int = -5;

/// `400` body for a NULL request pointer with a non-zero length.
pub const NULL_REQUEST_MSG: &str = "invalid request buffer (NULL with non-zero length)";
/// `503` body for an async dispatch dropped before it completed (runtime
/// shutdown).
pub const ASYNC_DROPPED_MSG: &str = "dispatch dropped before completion (runtime shut down)";
const PANIC_MSG: &str = "panic in Rust engine";

/// `vespera_callback` — completion callback of `vespera_dispatch_async`.
/// Receives ownership of the wire response (release with `vespera_free`).
pub type VesperaCallback =
    unsafe extern "C" fn(user_data: *mut c_void, response: *mut u8, response_len: usize);

/// Borrow the request buffer, or `None` for a NULL pointer with a non-zero
/// (or slice-impossible) length.
///
/// # Safety
///
/// When `len > 0` and `ptr` is non-NULL, `ptr` must be readable for `len`
/// bytes and not mutated for `'a`.
unsafe fn request_slice<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if ptr.is_null() || isize::try_from(len).is_err() {
        return None;
    }
    // SAFETY: non-NULL, `len <= isize::MAX`, readable per the contract.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Hand a wire response to the host: `(ptr, len)` of a leaked `Box<[u8]>`,
/// released by [`vespera_free`].
fn into_raw_wire(wire: Vec<u8>) -> (*mut u8, usize) {
    let boxed = wire.into_boxed_slice();
    let len = boxed.len();
    (Box::into_raw(boxed).cast::<u8>(), len)
}

/// Whether two non-empty address ranges overlap (empty ranges never do).
const fn ranges_overlap(a: usize, a_len: usize, b: usize, b_len: usize) -> bool {
    a_len != 0 && b_len != 0 && a < b.saturating_add(b_len) && b < a.saturating_add(a_len)
}

fn panic_wire() -> Vec<u8> {
    panic_error_wire(PANIC_MSG)
}

/// The wire response for a synchronous buffered dispatch of `input`.
fn dispatch_buffered(input: Option<&[u8]>) -> Vec<u8> {
    let Some(input) = input else {
        return error_wire(400, NULL_REQUEST_MSG);
    };
    check_ingress_cap(input.len()).unwrap_or_else(|| {
        block_on_shared_runtime(crate::dispatch_from_bytes_async(input.to_vec()))
            .unwrap_or_else(runtime_unavailable_wire)
    })
}

/// Shared body of `vespera_init` (emitted by [`ffi_apps!`](crate::ffi_apps)):
/// mark the host loaded, run `register`, build the shared runtime.
#[doc(hidden)]
pub fn __init(register: impl FnOnce() -> bool) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        crate::runtime::mark_host_loaded();
        if !register() {
            return VESPERA_ERR_REGISTRATION;
        }
        if runtime().is_none() {
            VESPERA_ERR_RUNTIME
        } else {
            VESPERA_OK
        }
    }))
    .unwrap_or(VESPERA_ERR_PANIC)
}

/// `uint32_t vespera_abi_version(void)`.
#[unsafe(no_mangle)]
pub extern "C" fn vespera_abi_version() -> u32 {
    VESPERA_ABI_VERSION
}

/// `int vespera_dispatch(const uint8_t*, size_t, uint8_t**, size_t*)` —
/// synchronous buffered dispatch; see `include/vespera.h`.
///
/// # Safety
///
/// `request` must be readable for `request_len` bytes (or `request_len ==
/// 0`); `response` / `response_len` must be NULL or valid for one write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vespera_dispatch(
    request: *const u8,
    request_len: usize,
    response: *mut *mut u8,
    response_len: *mut usize,
) -> c_int {
    if response.is_null() || response_len.is_null() {
        return VESPERA_ERR_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's contract on `request`.
    let wire = catch_unwind(|| dispatch_buffered(unsafe { request_slice(request, request_len) }))
        .unwrap_or_else(|_| panic_wire());
    let (ptr, len) = into_raw_wire(wire);
    // SAFETY: both non-NULL (checked above) and valid for a write per the
    // caller's contract.
    unsafe {
        response.write(ptr);
        response_len.write(len);
    }
    VESPERA_OK
}

/// `int vespera_dispatch_into(const uint8_t*, size_t, uint8_t*, size_t,
/// size_t*)` — synchronous dispatch into the caller's buffer; see
/// `include/vespera.h`.
///
/// # Safety
///
/// `request` must be readable for `request_len` bytes (or `request_len ==
/// 0`); `out` must be writable for `out_capacity` bytes (or
/// `out_capacity == 0`); `written` must be NULL or valid for one write.
/// The request and output buffers must not overlap (checked: overlapping
/// ranges are rejected).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vespera_dispatch_into(
    request: *const u8,
    request_len: usize,
    out: *mut u8,
    out_capacity: usize,
    written: *mut usize,
) -> c_int {
    if written.is_null()
        || (out.is_null() && out_capacity != 0)
        || isize::try_from(out_capacity).is_err()
        || ranges_overlap(request.addr(), request_len, out.addr(), out_capacity)
    {
        return VESPERA_ERR_INVALID_ARGUMENT;
    }
    let out: &mut [u8] = if out_capacity == 0 {
        &mut []
    } else {
        // SAFETY: non-NULL, `out_capacity <= isize::MAX`, writable per the
        // contract and disjoint from `request` (checked above).
        unsafe { std::slice::from_raw_parts_mut(out, out_capacity) }
    };
    // SAFETY: the caller's contract on `request`.
    let input = unsafe { request_slice(request, request_len) };
    let result = catch_unwind(AssertUnwindSafe(|| dispatch_direct(input, &mut *out)))
        .unwrap_or_else(|_| crate::direct::write_wire_into(out, &panic_wire()));
    let (code, n) = match result {
        DirectWriteResult::Complete(n) => (VESPERA_OK, n),
        DirectWriteResult::Overflow(required) => (VESPERA_OVERFLOW, required),
    };
    // SAFETY: non-NULL (checked above), valid for a write per the contract.
    unsafe { written.write(n) };
    code
}

/// The direct-write outcome for `input` into `out`.
fn dispatch_direct(input: Option<&[u8]>, out: &mut [u8]) -> DirectWriteResult {
    let Some(input) = input else {
        return crate::direct::write_wire_into(out, &error_wire(400, NULL_REQUEST_MSG));
    };
    if let Some(err) = check_ingress_cap(input.len()) {
        return crate::direct::write_wire_into(out, &err);
    }
    block_on_shared_runtime(crate::dispatch_into_async_borrowed(input, &mut *out))
        .unwrap_or_else(|| crate::direct::write_wire_into(out, &runtime_unavailable_wire()))
}

/// The host's completion target for one async dispatch.  Delivered exactly
/// once: explicitly, or — when the task is dropped first (runtime
/// shutdown) — with a `503` from `Drop`.
struct Completion {
    target: Option<(VesperaCallback, *mut c_void)>,
}

// SAFETY: the C contract requires `user_data` to be usable from any thread
// (documented on `vespera_dispatch_async`); the callback is a plain function
// pointer.
unsafe impl Send for Completion {}

impl Completion {
    fn deliver(&mut self, wire: Vec<u8>) {
        if let Some((callback, user_data)) = self.target.take() {
            let (ptr, len) = into_raw_wire(wire);
            // SAFETY: a host-supplied callback honouring the header's
            // contract; it takes ownership of the response.
            unsafe { callback(user_data, ptr, len) };
        }
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        self.deliver(error_wire(503, ASYNC_DROPPED_MSG));
    }
}

/// What a spawned async dispatch holds: the copied request, its in-flight
/// slot and its runtime lease.
type AsyncClaim = (Vec<u8>, AsyncPermit, RuntimeLease);

/// Admit (in-flight cap, shared with JNI `dispatchAsync`), validate + copy
/// the request and claim the runtime, or the wire response to deliver
/// inline.
fn prepare_async(input: Option<&[u8]>) -> Result<AsyncClaim, Vec<u8>> {
    let permit = try_acquire_async_permit().ok_or_else(async_overloaded_wire)?;
    let input = input.ok_or_else(|| error_wire(400, NULL_REQUEST_MSG))?;
    if let Some(err) = check_ingress_cap(input.len()) {
        return Err(err);
    }
    let input = input.to_vec();
    runtime()
        .map(|lease| (input, permit, lease))
        .ok_or_else(runtime_unavailable_wire)
}

/// The spawned async dispatch: poll it under `catch_unwind` (a handler
/// panic answers a `500` panic wire), deliver, then release the in-flight
/// slot and the lease so a shutdown drain waits for the callback too.
async fn run_async(
    input: Vec<u8>,
    permit: AsyncPermit,
    lease: RuntimeLease,
    mut completion: Completion,
) {
    let mut dispatch = Box::pin(crate::dispatch_from_bytes_async(input));
    let wire = std::future::poll_fn(|cx| {
        catch_unwind(AssertUnwindSafe(|| dispatch.as_mut().poll(cx)))
            .unwrap_or_else(|_| Poll::Ready(panic_wire()))
    })
    .await;
    completion.deliver(wire);
    drop((permit, lease));
}

/// `int vespera_dispatch_async(const uint8_t*, size_t, vespera_callback,
/// void*)` — see `include/vespera.h`.
///
/// # Safety
///
/// `request` must be readable for `request_len` bytes (or `request_len ==
/// 0`) for the duration of this call (it is copied before returning);
/// `callback` must be NULL or a function honouring the `vespera_callback`
/// contract; `user_data` must be usable from any thread until the callback
/// has run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vespera_dispatch_async(
    request: *const u8,
    request_len: usize,
    callback: Option<VesperaCallback>,
    user_data: *mut c_void,
) -> c_int {
    let Some(callback) = callback else {
        return VESPERA_ERR_INVALID_ARGUMENT;
    };
    let mut completion = Completion {
        target: Some((callback, user_data)),
    };
    // SAFETY: the caller's contract on `request`.
    let input = unsafe { request_slice(request, request_len) };
    match catch_unwind(|| prepare_async(input)).unwrap_or_else(|_| Err(panic_wire())) {
        Err(wire) => completion.deliver(wire),
        Ok((input, permit, lease)) => drop(
            lease
                .handle()
                .spawn(run_async(input, permit, lease, completion)),
        ),
    }
    VESPERA_OK
}

/// `void vespera_free(uint8_t*, size_t)` — release a response returned by
/// `vespera_dispatch` or handed to a `vespera_callback`.
///
/// # Safety
///
/// `ptr` must be NULL or a pointer returned by this library together with
/// `len`, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vespera_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        // SAFETY: `ptr`/`len` came from `into_raw_wire` (a leaked
        // `Box<[u8]>` of exactly `len` bytes) per the caller's contract.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
    }
}

/// `int vespera_shutdown(uint64_t timeout_ms)` — see `include/vespera.h`.
#[unsafe(no_mangle)]
pub extern "C" fn vespera_shutdown(timeout_ms: u64) -> c_int {
    catch_unwind(|| {
        if shutdown_runtime(Duration::from_millis(timeout_ms)) {
            VESPERA_OK
        } else {
            VESPERA_ERR_RUNTIME
        }
    })
    .unwrap_or(VESPERA_ERR_PANIC)
}

/// Export `vespera_init` registering one **default** app — sugar for
/// `ffi_apps! { "_default" => factory }`.  Use once, in the cdylib root.
///
/// ```ignore
/// vespera::ffi_app!(create_app);
/// ```
#[macro_export]
macro_rules! ffi_app {
    ($factory:expr) => {
        $crate::ffi_apps! { "_default" => $factory }
    };
}

/// Export `vespera_init` registering **named apps** (the C-ABI twin of
/// `jni_apps!`).  Names are checked at compile time (non-empty, ≤ 64 bytes,
/// `[A-Za-z0-9_-]`, unique); a request's wire `"app"` field selects one
/// (absent → `"_default"`).  Exactly one `ffi_app!` / `ffi_apps!` per cdylib
/// (a second one is a duplicate-symbol error).  `vespera_init` returns
/// `VESPERA_ERR_REGISTRATION` (reason on stderr) when a registration fails
/// and `VESPERA_ERR_PANIC` when a factory panics.
///
/// ```ignore
/// vespera::ffi_apps! {
///     "_default" => create_app,
///     "admin"    => admin_app,
/// }
/// ```
#[macro_export]
macro_rules! ffi_apps {
    ( $( $name:literal => $factory:expr ),+ $(,)? ) => {
        const _: () = {
            ::std::assert!($crate::__app_names_valid(&[$($name),+]), "ffi_apps!: every app name must be non-empty, at most 64 bytes, and only contain [A-Za-z0-9_-]");
            ::std::assert!($crate::__app_names_unique(&[$($name),+]), "ffi_apps!: duplicate app name — each app may be registered only once");
        };

        /// C ABI: register this cdylib's apps and start the shared runtime
        /// (idempotent).  See `vespera.h`.
        #[unsafe(no_mangle)]
        pub extern "C" fn vespera_init() -> ::std::ffi::c_int {
            $crate::ffi::__init(|| {
                $( if let ::std::result::Result::Err(err) = $crate::try_register_app_named($name, $factory) { ::std::eprintln!("vespera: ffi_apps! failed to register app {:?}: {}", $name, err); return false; } )+
                true
            })
        }
    };
}

#[cfg(test)]
mod tests {
    //! Pointer-contract checks that never start the runtime (Miri-friendly).
    use super::*;

    #[test]
    fn request_slice_contract() {
        // SAFETY: NULL/len-0 and NULL/len>0 never dereference; the last
        // case borrows a live array.
        unsafe {
            assert_eq!(request_slice(std::ptr::null(), 0), Some(&[][..]));
            assert_eq!(request_slice(std::ptr::null(), 3), None);
            assert_eq!(
                request_slice(std::ptr::NonNull::<u8>::dangling().as_ptr(), usize::MAX),
                None
            );
            let bytes = [1u8, 2, 3];
            assert_eq!(request_slice(bytes.as_ptr(), 2), Some(&bytes[..2]));
        }
    }

    #[test]
    fn overlap_rules() {
        assert!(ranges_overlap(10, 5, 14, 1));
        assert!(!ranges_overlap(10, 5, 15, 1));
        assert!(!ranges_overlap(10, 0, 10, 5));
        assert!(!ranges_overlap(10, 5, 12, 0));
        assert!(ranges_overlap(usize::MAX - 4, 2, usize::MAX - 5, 4));
    }

    #[test]
    fn raw_wire_round_trips_through_free() {
        let (ptr, len) = into_raw_wire(vec![7u8; 9]);
        // SAFETY: just leaked by `into_raw_wire`.
        unsafe {
            assert_eq!(std::slice::from_raw_parts(ptr, len), &[7u8; 9]);
            vespera_free(ptr, len);
            vespera_free(std::ptr::null_mut(), 0);
        }
        let (ptr, len) = into_raw_wire(Vec::new());
        // SAFETY: zero-length leaked box.
        unsafe { vespera_free(ptr, len) };
    }

    #[test]
    fn misuse_is_rejected_without_writing() {
        let mut out = std::ptr::null_mut();
        let mut out_len = 0usize;
        let mut buf = [0u8; 8];
        let mut written = 0usize;
        // SAFETY: every call is rejected before touching the buffers.
        unsafe {
            assert_eq!(
                vespera_dispatch(std::ptr::null(), 0, std::ptr::null_mut(), &raw mut out_len),
                VESPERA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                vespera_dispatch(std::ptr::null(), 0, &raw mut out, std::ptr::null_mut()),
                VESPERA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                vespera_dispatch_into(
                    std::ptr::null(),
                    0,
                    buf.as_mut_ptr(),
                    buf.len(),
                    std::ptr::null_mut()
                ),
                VESPERA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                vespera_dispatch_into(
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    8,
                    &raw mut written
                ),
                VESPERA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                vespera_dispatch_into(
                    std::ptr::null(),
                    0,
                    buf.as_mut_ptr(),
                    usize::MAX,
                    &raw mut written
                ),
                VESPERA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                vespera_dispatch_into(
                    buf.as_ptr(),
                    4,
                    buf.as_mut_ptr().add(2),
                    6,
                    &raw mut written
                ),
                VESPERA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                vespera_dispatch_async(std::ptr::null(), 0, None, std::ptr::null_mut()),
                VESPERA_ERR_INVALID_ARGUMENT
            );
        }
        assert!(out.is_null());
        assert_eq!((out_len, written), (0, 0));
        assert_eq!(vespera_abi_version(), VESPERA_ABI_VERSION);
    }
}
