//! Sound, zero-fill-free reads of a Java `byte[]` region into an owned
//! `Vec<u8>`.
//!
//! `JByteArray::get_region` (and `Env::convert_byte_array`) require an
//! already-initialised `&mut [i8]` destination, which forces a
//! `vec![0u8; len]` whose every byte is then immediately overwritten by
//! the JNI copy — wasted work that, on the streaming request path, runs
//! once per body chunk (≈ 4096 times for a 1 GiB / 256 KiB upload).
//!
//! This helper instead hands the raw `GetByteArrayRegion` JNI entry a
//! pointer into the `Vec`'s **uninitialised** spare capacity — exactly
//! how jni's own `convert_byte_array` calls `GetByteArrayRegion`
//! internally — and only `set_len`s after the copy succeeds.  No
//! `&mut [i8]` reference over uninitialised memory is ever created, so
//! there is no `slice::from_raw_parts_mut`-over-uninit UB (the precise
//! reason the previous code zero-filled first).
//!
//! The JNI-free halves — [`fill_spare`] (reserve, fill through a raw
//! pointer, `set_len`) and [`as_jbytes`] (the `u8` → `i8` view) — are the
//! only `unsafe` here that does not need a JVM, so they are unit-tested
//! under Miri.

use jni::objects::JByteArray;
use jni::sys::{jarray, jbyte, jsize};

/// Read `arr[0..len]` into a fresh `Vec<u8>` of length `len`, skipping
/// the zero-fill that `get_region` / `convert_byte_array` pay.
///
/// On any pending JNI exception (e.g. the array was concurrently shrunk
/// so the region is out of bounds) the exception is cleared and an
/// `Err` is returned with the `Vec` left **empty** — uninitialised bytes
/// are never observable.
pub fn read_byte_array_region(
    env: &mut jni::Env<'_>,
    arr: &JByteArray<'_>,
    len: usize,
) -> jni::errors::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: `fill_from_java` returns `Ok` only once `GetByteArrayRegion`
    // returned with no pending exception, i.e. after it initialised all
    // `len` bytes at `dst` — `fill_spare`'s contract.
    unsafe {
        fill_spare(
            len,
            || jni::errors::Error::JniCall(jni::errors::JniError::NoMemory),
            |dst| fill_from_java(env, arr, len, dst),
        )
    }
}

/// Build a `Vec<u8>` of exactly `len` bytes without zero-filling it:
/// reserve `len` bytes, let `fill` write them through a raw pointer into
/// the spare capacity, and only then `set_len`.
///
/// The reservation is fallible: a very large `len` (a multi-GB request that
/// passed an unlimited / loose ingress cap, or genuine memory pressure) must
/// NOT reach Rust's infallible-allocation OOM handler, which ABORTS the
/// process — and an abort takes down the host JVM, uncatchable by the
/// `catch_unwind` guards at the JNI entry points.  A failed reservation
/// returns `on_oom()` (the caller maps it to a wire `413`) before `fill` is
/// called.  When `fill` fails, the `Vec` is dropped at length 0, so no
/// uninitialised byte is ever observable.
///
/// # Safety
///
/// When `fill` returns `Ok(())` it must have initialised all `len` bytes
/// starting at the pointer it was given (valid for `len` writes, and only
/// for the duration of the call).
unsafe fn fill_spare<E>(
    len: usize,
    on_oom: impl FnOnce() -> E,
    fill: impl FnOnce(*mut u8) -> Result<(), E>,
) -> Result<Vec<u8>, E> {
    let mut vec: Vec<u8> = Vec::new();
    vec.try_reserve_exact(len).map_err(|_| on_oom())?;
    fill(vec.as_mut_ptr())?;
    // SAFETY: `try_reserve_exact(len)` made the capacity at least `len`, and
    // the caller's contract says `fill` returning `Ok` initialised all `len`
    // bytes.
    unsafe {
        vec.set_len(len);
    }
    Ok(vec)
}

/// Copy `arr[0..len]` into `dst` with `GetByteArrayRegion`; `Ok` only when
/// the copy completed without a pending exception.
fn fill_from_java(
    env: &mut jni::Env<'_>,
    arr: &JByteArray<'_>,
    len: usize,
    dst: *mut u8,
) -> jni::errors::Result<()> {
    // `GetByteArrayRegion` takes a `jsize` (i32) length.  `len` never
    // exceeds a Java array length (itself `jsize`-bounded), so this only
    // fails on a caller bug; surface it as an error rather than truncate.
    let region_len = jsize::try_from(len)
        .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments))?;

    let env_ptr = env.get_raw();
    let array = arr.as_raw();
    // SAFETY:
    // * `env_ptr` is the current thread's valid `JNIEnv`, returned by
    //   `Env::get_raw()`.  Dereferencing it to reach the JNI function
    //   table and invoking `GetByteArrayRegion` mirrors jni's own
    //   `convert_byte_array` (and `daemon_env`'s raw VM calls): the
    //   function-table entries are non-null `extern "system"` pointers.
    // * `array` is a live `byte[]` local/global reference; `[0, len)` is
    //   in bounds because callers pass either the array length (buffered
    //   path) or the exact positive `InputStream.read(byte[])` count after
    //   checking it does not exceed the fixed streaming buffer length.
    // * `dst` is `fill_spare`'s reserved-but-uninitialised capacity of at
    //   least `len` bytes.  Only a raw `*mut jbyte` is passed to JNI — no
    //   `&mut [i8]` over uninitialised memory is created.  `u8` and `jbyte`
    //   (`i8`) share size/alignment.
    unsafe {
        let interface = *env_ptr;
        ((*interface).v1_1.GetByteArrayRegion)(
            env_ptr,
            array as jarray,
            0,
            region_len,
            dst.cast::<jbyte>(),
        );
    }

    // `GetByteArrayRegion` only throws `ArrayIndexOutOfBoundsException`
    // for an out-of-range region; `[0, len)` is in range here, but check
    // defensively.  The `Err` makes `fill_spare` drop the `Vec` before
    // `set_len`, so no uninitialised byte is ever exposed.
    if env.exception_check() {
        env.exception_clear();
        return Err(jni::errors::Error::JavaException);
    }
    Ok(())
}

/// View bytes as the signed `jbyte` slice JNI's `set_region` expects.
pub fn as_jbytes(bytes: &[u8]) -> &[jbyte] {
    // SAFETY: `u8` and `i8` (JNI's `jbyte`) have identical size and
    // alignment and every bit pattern is valid for both; the view borrows
    // `bytes` for its whole lifetime and covers exactly its length.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<jbyte>(), bytes.len()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_spare_exposes_exactly_the_filled_bytes() {
        let src = b"vespera wire bytes";
        // SAFETY: the closure writes all `src.len()` bytes before `Ok`.
        let vec = unsafe {
            fill_spare(
                src.len(),
                || "oom",
                |dst| {
                    std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                    Ok(())
                },
            )
        }
        .unwrap();
        assert_eq!(vec, src);
        assert!(vec.capacity() >= src.len());
    }

    #[test]
    fn fill_spare_failure_never_exposes_uninit_bytes() {
        // A partial write followed by an error: the half-initialised buffer
        // must be dropped without being read (Miri flags any uninit read).
        // SAFETY: the closure returns `Err`, so nothing is assumed written.
        let result = unsafe {
            fill_spare(
                8,
                || "oom",
                |dst| {
                    dst.write(1);
                    Err("filled 1 of 8")
                },
            )
        };
        assert_eq!(result, Err("filled 1 of 8"));
    }

    #[test]
    fn fill_spare_maps_an_impossible_reservation_to_on_oom() {
        let mut called = false;
        // SAFETY: never reached — the reservation fails first.
        let result = unsafe {
            fill_spare(
                usize::MAX,
                || "oom",
                |_| {
                    called = true;
                    Ok(())
                },
            )
        };
        assert_eq!(result, Err("oom"));
        assert!(!called, "fill must not run without a reservation");
    }

    #[test]
    fn as_jbytes_is_a_bitwise_view() {
        let bytes = [0x00, 0x7F, 0x80, 0xFF];
        assert_eq!(as_jbytes(&bytes), &[0, 127, -128, -1]);
        assert_eq!(as_jbytes(&[]), &[] as &[jbyte]);
    }
}
