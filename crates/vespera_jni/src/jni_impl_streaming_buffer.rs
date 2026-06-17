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
        let replace_cached = slot
            .as_ref()
            .is_none_or(|cached| cached.size != size && !cached.checked_out);

        if replace_cached {
            *slot = Some(CachedStreamingChunkBuffer {
                size,
                array: new_streaming_chunk_buffer(env, size)?,
                checked_out: false,
            });
        }

        let Some(cached) = slot.as_mut() else {
            return Ok((new_streaming_chunk_buffer(env, size)?, None));
        };

        if cached.size != size || cached.checked_out {
            return Ok((new_streaming_chunk_buffer(env, size)?, None));
        }

        let cached_array: &JByteArray<'static> = cached.array.as_ref();
        let dispatch_array = env.new_global_ref(cached_array)?;
        cached.checked_out = true;
        Ok((dispatch_array, Some(StreamingChunkBufferLease::new(role))))
    })
}

pub fn mark_streaming_buffer_reusable(lease: Option<StreamingChunkBufferLease>) {
    if let Some(lease) = lease {
        lease.mark_reusable();
    }
}
