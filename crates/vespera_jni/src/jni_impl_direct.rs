//! Direct-buffer (zero JNI region copy) synchronous dispatch.
//!
//! The `dispatchDirect0` JNI symbol and its helpers, split out of
//! `jni_impl.rs` to keep that file within the project's 1000-line
//! source cap.  Semantics are unchanged; `block_on_sync_runtime` is
//! reused from the parent module.

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteBuffer, JClass};
use jni::sys::jint;

use super::{block_on_sync_runtime, panic_wire};

/// Sentinel for [`Java_..._dispatchDirect`]: the response (or its
/// required size) cannot be represented in the `jint` return value
/// (> `i32::MAX` bytes).
///
/// `jint::MIN` is the only value the `-(required_size)` protocol can
/// never produce: `required_size <= i32::MAX`, so the most negative
/// legitimate return is `-(i32::MAX) == jint::MIN + 1`.
const DIRECT_UNREPRESENTABLE: jint = jint::MIN;

// Compile-time proof that the sentinel cannot collide with any
// legitimate `-(required_size)` value.
const _: () = assert!(DIRECT_UNREPRESENTABLE < -i32::MAX);

/// Copy `response` into the caller's direct out buffer.
///
/// Returns:
/// * `>= 0` — bytes written (`response` fit entirely)
/// * `< 0`  — `-(required_size)`: nothing written, caller must retry
///   with a buffer of at least `required_size` bytes
/// * [`DIRECT_UNREPRESENTABLE`] — response exceeds `i32::MAX` bytes
///   and cannot be expressed in the return-code protocol
///
/// # Safety contract (upheld by the caller)
///
/// `out_addr` must point to a writable region of at least `out_cap`
/// bytes that stays valid for the duration of this call (a JNI
/// direct buffer pinned by the live `JByteBuffer` local ref).
/// Whether `[a0, a0+a_len)` and `[b0, b0+b_len)` overlap (addresses as
/// `usize`).  Used to reject aliasing `in_buf` / `out_buf` direct-buffer
/// ranges in [`Java_..._dispatchDirect0`] before creating a shared `&[u8]`
/// and an exclusive `&mut [u8]` over them (SEC-1).  `saturating_add`
/// keeps the bound arithmetic panic-free for any address.
fn ranges_overlap(a0: usize, a_len: usize, b0: usize, b_len: usize) -> bool {
    let a1 = a0.saturating_add(a_len);
    let b1 = b0.saturating_add(b_len);
    a0 < b1 && b0 < a1
}

/// Copy `response` into the caller's direct out buffer, returning the
/// `dispatchDirect0` code (`>= 0` bytes written, `-(required)` on overflow,
/// [`DIRECT_UNREPRESENTABLE`] when the size exceeds `i32::MAX`).
///
/// # Safety
///
/// `out_addr` must point to a writable region of at least `out_cap` bytes
/// that stays valid for the whole call (a JNI direct buffer pinned by a
/// live `JByteBuffer` local ref) and must NOT alias `response` (callers
/// pass a Rust-owned wire `Vec`).  Encoded as `unsafe fn` so every call
/// site acknowledges the raw-pointer contract instead of it being an
/// unchecked promise on a safe function.
unsafe fn write_response_to_out(out_addr: *mut u8, out_cap: usize, response: &[u8]) -> jint {
    if response.len() <= out_cap {
        // SAFETY: `response.len() <= out_cap` and the caller's `# Safety`
        // contract guarantees `out_addr..out_addr+out_cap` is writable and
        // non-aliasing with `response` (a Rust-owned Vec → a Java direct
        // buffer).
        unsafe {
            std::ptr::copy_nonoverlapping(response.as_ptr(), out_addr, response.len());
        }
        // Java buffer capacities are jint-bounded, so len <= cap
        // always fits i32.
        jint::try_from(response.len()).unwrap_or(DIRECT_UNREPRESENTABLE)
    } else {
        jint::try_from(response.len()).map_or(DIRECT_UNREPRESENTABLE, |required| -required)
    }
}

/// `com.devfive.vespera.bridge.VesperaBridge.dispatchDirect0(ByteBuffer, int, ByteBuffer) -> int`
/// (private native; the public Java wrapper `dispatchDirect` validates
/// buffer directness and writability before crossing JNI)
///
/// **Direct-buffer** synchronous dispatch — the zero-JNI-region-copy
/// sibling of [`Java_...dispatchBytes`].
///
/// Contract (mirrored in the Java wrapper's javadoc):
/// * `in_buf` / `out_buf` MUST be **direct, writable** `ByteBuffer`s.
///   The public Java wrapper is the authoritative guard: it rejects
///   non-direct and read-only buffers before crossing JNI.  This private
///   native symbol deliberately does NOT call back into Java (for example,
///   `ByteBuffer.isReadOnly()`) because this ~2 µs direct path is selected
///   specifically to avoid per-request JNI calls beyond raw-address/capacity
///   resolution.  Callers that bypass the Java wrapper violate this ABI
///   contract and may hand Rust a read-only page as `&mut [u8]`.
/// * The wire request is read from `in_buf[0..in_len]` — explicit
///   `in_len`, **never** the buffer's position/limit (eliminates
///   the classic "forgot to flip()" corruption).
/// * Return `>= 0`: a complete wire response was written to
///   `out_buf[0..n]`.
/// * Return `< 0`: `-(required_size)` — the response did not fit.
///   `out_buf` contents are **undefined** (a prefix may have been
///   written).  `required_size` is exact, but retrying re-runs the
///   dispatch, so the Java side only auto-retries idempotent
///   methods.
/// * `Integer.MIN_VALUE`: response size exceeds `i32::MAX`.
///
/// Compared with `dispatchBytes`, this path removes BOTH JNI
/// region copies (Java `byte[]` ↔ Rust), the per-call Java heap
/// array allocations, AND — via
/// [`vespera_inprocess::dispatch_into_async_borrowed`] — the
/// intermediate response `Vec` AND the request-side input copy: the
/// wire header is parsed **in place** from the borrowed `in_buf`, and
/// only a non-empty request body is copied into an owned `Bytes`
/// (axum's `Body` requires `'static` ownership), so a bodyless `GET`
/// copies nothing on the request side.  On the success path the wire
/// header and each body frame are written straight into `out_buf`.
/// `422` responses are materialised internally to preserve
/// `validation_errors` hoisting.
///
/// # Safety invariants (comment-locked)
///
/// 1. `in_buf` / `out_buf` stay rooted as live local refs for the
///    whole call — HotSpot neither moves nor frees the backing
///    memory of a direct buffer while its object is reachable.
/// 2. The raw addresses derived from them are used **only within
///    this function body** — never captured by closures, spawned
///    tasks, or returned structs.
/// 3. The input is read through a **borrowed** slice for the duration
///    of the synchronous `block_on` (no `Vec` copy).  Invariant 1
///    keeps the backing memory valid throughout and the borrow never
///    escapes the `block_on`, so nothing borrowed from the buffer
///    outlives the call.
/// 4. `in_buf` and `out_buf` are proven **non-overlapping** (SEC-1)
///    before the shared `&[u8]` / exclusive `&mut [u8]` are created, so
///    they never alias the same memory.
/// 5. `out_buf` is **writable** and covers at least `out_cap` bytes.  This is
///    an explicit ABI precondition of this private symbol, enforced by the
///    public Java wrapper's `isReadOnly()` checks (SEC-2).  Re-checking here
///    would add a hot-path JNI call, so the native side documents and trusts
///    that wrapper contract for speed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_devfive_vespera_bridge_VesperaBridge_dispatchDirect0<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    in_buf: JByteBuffer<'local>,
    in_len: jint,
    out_buf: JByteBuffer<'local>,
) -> jint {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jint> {
            let mut out_region: Option<(*mut u8, usize)> = None;
            let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> jni::errors::Result<jint> {
                    // Resolve the OUTPUT buffer FIRST and record it, so any
                    // *later* failure (notably an invalid `in_buf`) can still
                    // write a decodable wire response into it instead of
                    // throwing — upholding the dispatch* family contract that
                    // every failure yields a wire response.  An output-resolution
                    // failure (null ⇒ heap buffer, or JVM trouble) has no buffer
                    // to write into, so it still propagates via `?` → the
                    // RuntimeException the resolve below maps it to (defense in
                    // depth behind the Java-side isDirect()/isReadOnly() guard).
                    let out_addr = env.get_direct_buffer_address(&out_buf)?;
                    let out_cap = env.get_direct_buffer_capacity(&out_buf)?;
                    out_region = Some((out_addr, out_cap));
                    debug_assert!(
                        !out_addr.is_null(),
                        "JNI direct output buffer address must be non-null"
                    );

                    // Now resolve the INPUT buffer.  A failure here (null ⇒ heap
                    // buffer, non-direct, or JVM trouble) writes a `400` wire
                    // response into the already-resolved output buffer instead of
                    // throwing + returning the default `jint` — so a caller that
                    // bypasses the Java wrapper with a bad `in_buf` but a valid
                    // `out_buf` still receives a decodable wire error.
                    let in_resolved = match env.get_direct_buffer_address(&in_buf) {
                        Ok(addr) => env.get_direct_buffer_capacity(&in_buf).map(|cap| (addr, cap)),
                        Err(e) => Err(e),
                    };
                    let Ok((in_addr, in_cap)) = in_resolved else {
                        // GetDirectBufferAddress returns NULL without raising a
                        // Java exception, but clear defensively so the wire
                        // response is delivered with no exception in flight.
                        if env.exception_check() {
                            env.exception_clear();
                        }
                        let err = vespera_inprocess::error_wire(
                            400,
                            "invalid in_buf (null, heap, or non-direct ByteBuffer)",
                        );
                        // SAFETY: `out_addr`/`out_cap` came from the live direct
                        // output buffer above and `err` is a Rust-owned Vec.
                        return Ok(unsafe { write_response_to_out(out_addr, out_cap, &err) });
                    };

                    // Validate in_len against the buffer's real capacity —
                    // all failures still produce a valid wire response in
                    // `out_buf`, per the dispatch* family contract.
                    let in_len = match usize::try_from(in_len) {
                        Ok(len) if len <= in_cap => len,
                        _ => {
                            let err = vespera_inprocess::error_wire(
                                400,
                                "invalid in_len (negative or exceeds buffer capacity)",
                            );
                            // SAFETY: `out_addr`/`out_cap` came from the live direct
                            // output buffer above and `err` is a Rust-owned Vec.
                            return Ok(unsafe { write_response_to_out(out_addr, out_cap, &err) });
                        }
                    };

                    // SEC-1: reject overlapping `in_buf` / `out_buf` ranges.
                    // Below we create a shared `&[u8]` over the input and an
                    // exclusive `&mut [u8]` over the output; if they alias the
                    // same direct-buffer memory (the caller passed the same
                    // buffer, or overlapping `slice()`/`duplicate()` views) that
                    // is instant UB.  The Java wrapper cannot detect this (it has
                    // no native address), so the check lives here.  `out_buf` is
                    // writable by the wrapper's `isReadOnly()` guard (SEC-2), so
                    // writing the error response into it is sound.
                    if ranges_overlap(in_addr as usize, in_len, out_addr as usize, out_cap) {
                        let err = vespera_inprocess::error_wire(
                            400,
                            "in_buf and out_buf must not overlap (aliasing would be undefined behavior)",
                        );
                        // SAFETY: `out_addr`/`out_cap` came from the live direct
                        // output buffer above and `err` is a Rust-owned Vec.
                        return Ok(unsafe { write_response_to_out(out_addr, out_cap, &err) });
                    }

                    let dispatched = {
                        // SAFETY: invariants 1–3 above.  `in_addr..in_addr+in_len`
                        // (`in_len <= in_cap`) is a readable region and
                        // `out_addr..out_addr+out_cap` a writable region, both of
                        // direct buffers pinned by their live `in_buf` / `out_buf`
                        // local refs; SEC-1 proved non-overlap and SEC-2 is the ABI
                        // contract that `out_buf` is writable (enforced by Java's
                        // public wrapper, not re-checked here to keep the direct hot
                        // path free of an extra JNI call). The Java caller is blocked
                        // for the whole call, so both buffers stay valid throughout.
                        // The borrowed `input` slice is read in place (no `Vec` copy)
                        // and never escapes this synchronous `block_on`.
                        let input = unsafe { std::slice::from_raw_parts(in_addr, in_len) };
                        let out = unsafe { std::slice::from_raw_parts_mut(out_addr, out_cap) };
                        block_on_sync_runtime(vespera_inprocess::dispatch_into_async_borrowed(
                            input, out,
                        ))
                    };

                    let code = match dispatched {
                        vespera_inprocess::DirectWriteResult::Complete(n) => {
                            // n <= out_cap, and Java buffer capacities are
                            // jint-bounded, so this always fits i32.
                            jint::try_from(n).unwrap_or(DIRECT_UNREPRESENTABLE)
                        }
                        vespera_inprocess::DirectWriteResult::Overflow(required) => {
                            jint::try_from(required).map_or(DIRECT_UNREPRESENTABLE, |r| -r)
                        }
                    };
                    Ok(code)
                },
            ));

            guarded.unwrap_or_else(|_| {
                out_region.map_or_else(
                    || {
                        let _ = env.throw_new(
                            jni::jni_str!("java/lang/RuntimeException"),
                            jni::jni_str!(
                                "panic in Rust engine before direct output buffer resolution"
                            ),
                        );
                        Ok(DIRECT_UNREPRESENTABLE)
                    },
                    |(out_addr, out_cap)| {
                        let err = panic_wire();
                        // SAFETY: `out_addr`/`out_cap` were resolved from the live
                        // direct output buffer before the panic, and `err` is a
                        // Rust-owned Vec that cannot alias that Java buffer.
                        Ok(unsafe { write_response_to_out(out_addr, out_cap, &err) })
                    },
                )
            })
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[cfg(test)]
#[path = "jni_impl_direct_tests.rs"]
mod direct_tests;
