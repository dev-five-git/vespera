//! Per-thread reusable Java byte-array buffers for the streaming JNI
//! dispatch paths.
//!
//! Split out of `jni_impl.rs` to keep that file within the project's
//! 1000-line source cap.  Semantics are unchanged: each streaming
//! direction (request pull / response push) keeps one cached
//! `Global<JByteArray>` of the configured chunk size, leased for the
//! duration of a dispatch and marked reusable only after the streaming
//! future returns normally (a panic leaves the lease checked out so the
//! next dispatch allocates a fresh buffer instead of aliasing the Java
//! array that may still be in flight).

use std::cell::RefCell;

use jni::objects::{Global, JByteArray};

use super::streaming_chunk_size;

thread_local! {
    static STREAMING_PULL_BUFFER: RefCell<Option<CachedStreamingChunkBuffer>> = const { RefCell::new(None) };
    static STREAMING_PUSH_BUFFER: RefCell<Option<CachedStreamingChunkBuffer>> = const { RefCell::new(None) };
}

pub type StreamingChunkBuffer = Global<JByteArray<'static>>;

#[derive(Clone, Copy)]
pub enum StreamingBufferRole {
    Pull,
    Push,
}

impl StreamingBufferRole {
    fn with_cache<R>(
        self,
        callback: impl FnOnce(&RefCell<Option<CachedStreamingChunkBuffer>>) -> R,
    ) -> R {
        match self {
            Self::Pull => STREAMING_PULL_BUFFER.with(callback),
            Self::Push => STREAMING_PUSH_BUFFER.with(callback),
        }
    }
}

struct CachedStreamingChunkBuffer {
    size: usize,
    array: StreamingChunkBuffer,
    checked_out: bool,
}

// Released explicitly only after the streaming future returns normally.  If a
// panic unwinds through a bidirectional dispatch while the request producer may
// still be in `InputStream.read`, the cache stays checked out and future
// dispatches allocate fresh buffers instead of aliasing the Java array.
pub struct StreamingChunkBufferLease {
    role: StreamingBufferRole,
}

impl StreamingChunkBufferLease {
    const fn new(role: StreamingBufferRole) -> Self {
        Self { role }
    }

    fn mark_reusable(self) {
        self.role.with_cache(|cache| {
            if let Some(cached) = cache.borrow_mut().as_mut() {
                cached.checked_out = false;
            }
        });
    }
}

fn new_streaming_chunk_buffer(
    env: &mut jni::Env<'_>,
    size: usize,
) -> jni::errors::Result<StreamingChunkBuffer> {
    let local = env.new_byte_array(size)?;
    env.new_global_ref(&local)
}

pub fn checkout_streaming_chunk_buffer(
    env: &mut jni::Env<'_>,
    role: StreamingBufferRole,
) -> jni::errors::Result<(StreamingChunkBuffer, Option<StreamingChunkBufferLease>)> {
    let size = streaming_chunk_size();
    role.with_cache(|cache| {
        let mut slot = cache.borrow_mut();
        // Three outcomes, decided by the cached slot's state:
        match slot.as_mut() {
            // Still checked out — a concurrent dispatch holds it, or a prior
            // dispatch panicked mid-stream and never returned its lease. Hand
            // back a throwaway, unpooled buffer and leave the cache untouched
            // so we never alias a Java array that may still be in flight.
            Some(cached) if cached.checked_out => {
                return Ok((new_streaming_chunk_buffer(env, size)?, None));
            }
            // Free to reuse — refresh the backing array only if the configured
            // chunk size changed, then lease it back to the caller.
            Some(cached) => {
                if cached.size != size {
                    cached.array = new_streaming_chunk_buffer(env, size)?;
                    cached.size = size;
                }
                let cached_array: &JByteArray<'static> = cached.array.as_ref();
                let dispatch_array = env.new_global_ref(cached_array)?;
                cached.checked_out = true;
                return Ok((dispatch_array, Some(StreamingChunkBufferLease::new(role))));
            }
            // Empty slot — fall through to install a fresh cached buffer.
            None => {}
        }
        let array = new_streaming_chunk_buffer(env, size)?;
        let array_ref: &JByteArray<'static> = array.as_ref();
        let dispatch_array = env.new_global_ref(array_ref)?;
        *slot = Some(CachedStreamingChunkBuffer {
            size,
            array,
            checked_out: true,
        });
        Ok((dispatch_array, Some(StreamingChunkBufferLease::new(role))))
    })
}

pub fn mark_streaming_buffer_reusable(lease: Option<StreamingChunkBufferLease>) {
    if let Some(lease) = lease {
        lease.mark_reusable();
    }
}

/// The pull + push per-thread chunk buffers (and their leases) acquired
/// together for one bidirectional streaming dispatch.
pub struct PullPushBuffers {
    pub pull_buf: StreamingChunkBuffer,
    pub pull_buf_lease: Option<StreamingChunkBufferLease>,
    pub push_buf: StreamingChunkBuffer,
    pub push_buf_lease: Option<StreamingChunkBufferLease>,
}

/// Check out the pull + push chunk buffers for a bidirectional stream in
/// one step.  Pull and push run concurrently on different threads, so each
/// direction gets its own per-thread cached buffer.
///
/// If the push checkout fails after the pull buffer was already leased, the
/// pull lease is released before returning the error so a half-acquired pair
/// never leaks a leased buffer (which would force the next dispatch to
/// allocate a fresh array).  Centralising this cleanup keeps the invariant in
/// one place instead of duplicating it across every bidirectional entry point.
pub fn checkout_pull_push_buffers(env: &mut jni::Env<'_>) -> jni::errors::Result<PullPushBuffers> {
    let (pull_buf, pull_buf_lease) = checkout_streaming_chunk_buffer(env, StreamingBufferRole::Pull)?;
    let (push_buf, push_buf_lease) =
        match checkout_streaming_chunk_buffer(env, StreamingBufferRole::Push) {
            Ok(checked_out) => checked_out,
            Err(err) => {
                mark_streaming_buffer_reusable(pull_buf_lease);
                return Err(err);
            }
        };
    Ok(PullPushBuffers {
        pull_buf,
        pull_buf_lease,
        push_buf,
        push_buf_lease,
    })
}
