/*
 * vespera.h — stable C ABI for embedding a vespera (Rust/axum) app
 * in-process.  Hand-written; `crates/vespera_inprocess/tests/ffi_header.rs`
 * keeps it in sync with the Rust symbols.
 *
 * The Rust cdylib enables `vespera = { features = ["ffi"] }` and invokes
 * `vespera::ffi_app!(create_app);` (or `vespera::ffi_apps! { ... }`), which
 * exports `vespera_init`; every other symbol below comes from the library.
 *
 * WIRE FORMAT (request and response):
 *   bytes 0..4   : u32 big-endian = header JSON length N
 *   bytes 4..4+N : UTF-8 JSON
 *                  request : {"v":1,"method","path","query"?,"headers"?,
 *                             "app"?,"remote_addr"?,"scheme"?,"principal"?}
 *                  response: {"v":1,"status","headers","metadata",
 *                             "validation_errors"?}
 *   bytes 4+N..  : raw body bytes
 * Every failure that can be expressed as HTTP (malformed wire 400, unknown
 * app 404, body too large 413, handler panic 500 with metadata
 * "panicked":"true", runtime shut down 503, deadline 504) is returned as a
 * normal wire response with VESPERA_OK.  Negative return codes are reserved
 * for API misuse and conditions where no response can be produced.
 *
 * THREAD SAFETY: every function may be called concurrently from any number
 * of host threads.  The synchronous dispatch functions block the calling
 * thread on the shared Tokio runtime; do not call them from a callback that
 * runs on a runtime worker (vespera_dispatch_async completions) — they
 * would answer a 500 panic response instead of deadlocking.
 *
 * PANICS never unwind across this ABI.
 */
#ifndef VESPERA_H
#define VESPERA_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any incompatible change to this header (adding functions or
 * return codes is compatible and does not bump it). */
#define VESPERA_ABI_VERSION 1

/* Return codes. */
#define VESPERA_OK 0
/* vespera_dispatch_into: the response did not fit; *written = required size. */
#define VESPERA_OVERFLOW 1
/* A required pointer was NULL, a size was out of range, or buffers overlap. */
#define VESPERA_ERR_INVALID_ARGUMENT (-1)
/* A Rust panic was caught where no wire response could be returned. */
#define VESPERA_ERR_PANIC (-2)
/* vespera_init: an app could not be registered (see stderr). */
#define VESPERA_ERR_REGISTRATION (-3)
/* vespera_init: runtime unavailable; vespera_shutdown: already shut down. */
#define VESPERA_ERR_RUNTIME (-4)
/* vespera_configure_*: the setting was already fixed; nothing changed. */
#define VESPERA_ERR_TOO_LATE (-5)

/*
 * Completion callback of vespera_dispatch_async.  Invoked at most once
 * (exactly once except for the vespera_shutdown case below), possibly
 * before vespera_dispatch_async returns: inline on the calling thread when
 * the request is rejected before dispatch (400 / 413 / 503), otherwise on a
 * runtime worker thread — including when
 * vespera_shutdown drops a pending dispatch with a 503, which can happen
 * after vespera_shutdown has returned.  The callback OWNS `response` and
 * must release it with vespera_free(response, response_len).  It must not
 * unwind.
 */
typedef void (*vespera_callback)(void *user_data, uint8_t *response, size_t response_len);

/* VESPERA_ABI_VERSION the library was built with. */
uint32_t vespera_abi_version(void);

/*
 * Register the app(s) declared by ffi_app!/ffi_apps! and start the shared
 * runtime.  Idempotent: later calls return VESPERA_OK without re-running
 * the factories.  Returns VESPERA_ERR_REGISTRATION, VESPERA_ERR_PANIC (a
 * factory panicked) or VESPERA_ERR_RUNTIME (shut down / could not start).
 * Exported by the ffi_app!/ffi_apps! macro in the user's cdylib.
 */
int vespera_init(void);

/*
 * Synchronous dispatch.  `request` must be readable for `request_len`
 * bytes (may be NULL when request_len == 0).  On VESPERA_OK, *response /
 * *response_len hold a Rust-allocated wire response the caller owns and
 * must release with vespera_free (`request` NULL with a non-zero
 * `request_len`, or `request_len` above PTRDIFF_MAX, is answered with a 400
 * wire response).  VESPERA_ERR_INVALID_ARGUMENT when `response` or
 * `response_len` is NULL (nothing is written).
 */
int vespera_dispatch(const uint8_t *request, size_t request_len, uint8_t **response, size_t *response_len);

/*
 * Synchronous dispatch writing the wire response straight into the
 * caller's buffer (no Rust allocation for the response).  VESPERA_OK:
 * *written = bytes written.  VESPERA_OVERFLOW: the response needs *written
 * bytes; `out` contents are undefined and a retry RE-RUNS the handler
 * (out == NULL with out_capacity == 0 is a valid size probe).
 * VESPERA_ERR_INVALID_ARGUMENT: `written` is NULL, `out` is NULL with a
 * non-zero capacity, or the request and output buffers overlap.
 */
int vespera_dispatch_into(const uint8_t *request, size_t request_len, uint8_t *out, size_t out_capacity, size_t *written);

/*
 * Asynchronous dispatch: returns without waiting for the handler;
 * `callback(user_data, ...)` receives the wire response once.  It runs
 * INLINE on this thread, before vespera_dispatch_async returns, when the
 * request is rejected before dispatch: too many async dispatches in flight
 * → 503 (see vespera_configure_async_max_in_flight), `request` NULL with a
 * non-zero `request_len` or `request_len` above PTRDIFF_MAX → 400, request
 * above the size cap → 413 (see vespera_configure_max_request_bytes),
 * runtime shut down → 503.  Otherwise it runs on a runtime worker, which
 * may also happen before vespera_dispatch_async returns: do not hold a
 * lock across this call that the callback takes, and have `user_data`
 * ready before calling.  `user_data` is passed through untouched and must
 * be usable from any thread.  VESPERA_ERR_INVALID_ARGUMENT (callback never
 * called) when `callback` is NULL.
 */
int vespera_dispatch_async(const uint8_t *request, size_t request_len, vespera_callback callback, void *user_data);

/*
 * Release a response returned by vespera_dispatch or handed to a
 * vespera_callback.  `len` must be the length returned with it.  NULL is a
 * no-op.  Never pass memory from another allocator.
 */
void vespera_free(uint8_t *ptr, size_t len);

/*
 * Shut the shared runtime down for good: new dispatches answer 503 at
 * once, in-flight ones get up to `timeout_ms` to finish, then the runtime
 * stops.  Pending async dispatches are dropped and their callbacks receive
 * a 503 on a runtime worker thread, possibly after this call returned.
 * EXCEPTION: a handler still blocking its worker thread (not yielding at an
 * .await) when the time is up is abandoned with the runtime: its callback
 * never runs and its `user_data` is never released.  Returns VESPERA_OK,
 * or VESPERA_ERR_RUNTIME when it was already shut down.  Irreversible for
 * the process.
 */
int vespera_shutdown(uint64_t timeout_ms);

/*
 * Process-wide settings, the C twins of the JVM bridge's configure hooks
 * and of the VESPERA_* environment variables (a configure call wins over
 * the variable).  Call them BEFORE vespera_init: the runtime settings are
 * fixed when vespera_init starts the shared runtime, the others when a
 * dispatch first reads them.  Each returns VESPERA_OK when applied,
 * VESPERA_ERR_TOO_LATE when the setting was already fixed (an earlier call,
 * the runtime started, or a dispatch read it; nothing changes), or
 * VESPERA_ERR_PANIC.
 */

/* Runtime worker threads (VESPERA_RUNTIME_WORKERS), clamped to 1..1024. */
int vespera_configure_worker_threads(size_t worker_threads);

/* Runtime blocking-pool cap (VESPERA_RUNTIME_MAX_BLOCKING_THREADS),
 * clamped to 1..8192; default 512. */
int vespera_configure_max_blocking_threads(size_t max_blocking_threads);

/* Request size cap (VESPERA_MAX_REQUEST_BYTES) → 413; 0 = unlimited;
 * default 64 MiB. */
int vespera_configure_max_request_bytes(size_t max_bytes);

/* Buffered response cap (VESPERA_MAX_BUFFERED_RESPONSE_BYTES) → 500;
 * 0 = unlimited; default 256 MiB. */
int vespera_configure_max_buffered_response_bytes(size_t max_bytes);

/* Per-dispatch deadline (VESPERA_DISPATCH_TIMEOUT_MS) → 504; 0 = disabled
 * (the default). */
int vespera_configure_dispatch_timeout_ms(uint64_t timeout_ms);

/* Cap on in-flight vespera_dispatch_async calls, shared with the JVM
 * bridge's dispatchAsync (VESPERA_ASYNC_MAX_IN_FLIGHT) → 503; 0 = unlimited;
 * default 4096. */
int vespera_configure_async_max_in_flight(size_t max_in_flight);

#ifdef __cplusplus
}
#endif

#endif /* VESPERA_H */
