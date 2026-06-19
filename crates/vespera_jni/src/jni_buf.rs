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
    let mut vec: Vec<u8> = Vec::with_capacity(len);
    if len == 0 {
        return Ok(vec);
    }
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
    // * The destination is `vec`'s reserved-but-uninitialised capacity
    //   (`with_capacity(len)` reserved exactly `len` bytes).  Only a raw
    //   `*mut jbyte` is passed to JNI — no `&mut [i8]` over uninitialised
    //   memory is created.  `u8` and `jbyte` (`i8`) share size/alignment.
    unsafe {
        let interface = *env_ptr;
        ((*interface).v1_1.GetByteArrayRegion)(
            env_ptr,
            array as jarray,
            0,
            region_len,
            vec.as_mut_ptr().cast::<jbyte>(),
        );
    }

    // `GetByteArrayRegion` only throws `ArrayIndexOutOfBoundsException`
    // for an out-of-range region; `[0, len)` is in range here, but check
    // defensively.  Returning before `set_len` keeps the `Vec` empty so
    // no uninitialised byte is ever exposed.
    if env.exception_check() {
        env.exception_clear();
        return Err(jni::errors::Error::JavaException);
    }

    // SAFETY: `GetByteArrayRegion` returned with no pending exception, so
    // it initialised all `len` destination bytes.
    unsafe {
        vec.set_len(len);
    }
    Ok(vec)
}
