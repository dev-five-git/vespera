//! App registry: named `Router` factories with a lock-free
//! `OnceLock` fast path for the default app.

use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock, RwLock};

use crate::Router;
use crate::wire::{WireRequestHeader, error_wire};

/// Canonical name of the default app — used when the wire header
/// omits `"app"` or sets it to an empty string, and when callers use
/// the BC [`register_app`] entry point.
pub const DEFAULT_APP_NAME: &str = "_default";

/// Maximum allowed length of an app name (after trimming).  Sized so
/// names fit comfortably in URL path segments and log lines.
const MAX_APP_NAME_LEN: usize = 64;

// ── App Factory (shared FFI pattern) ─────────────────────────────────

/// Per-name router cache.  Indexed by app name; the default app uses
/// [`DEFAULT_APP_NAME`] (`"_default"`).
///
/// Uses [`RwLock`] (not [`OnceLock`]) so multiple named apps can be
/// registered after init time, while keeping dispatch reads
/// contention-free.  The map is read on every dispatch and written
/// only during `register_app*` calls (typically at process startup).
///
/// Lock poisoning recovery: every read path uses
/// `unwrap_or_else(|e| e.into_inner())` so a panic in a producer
/// thread does not lock out the dispatch hot path.  Factory closures
/// are also invoked **outside** the write lock so a factory panic
/// cannot poison the map.
static APP_ROUTERS: LazyLock<RwLock<HashMap<String, Router>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Lock-free fast path for the **default** app.
///
/// The overwhelmingly common dispatch case is a wire header without
/// an `"app"` field — routing to [`DEFAULT_APP_NAME`].  Resolving it
/// through `APP_ROUTERS` costs an `RwLock` read acquisition per
/// request, which parks threads under high concurrency.  This
/// `OnceLock` mirror is set (exactly once, inside the registration
/// write lock so it can never diverge from the map) by the first
/// successful `_default` registration and read with a single atomic
/// load + `Router::clone` (`Arc` refcount bump) on every dispatch.
///
/// Named apps keep using the `RwLock<HashMap>` — they are the rare
/// multi-app case and can be registered at any time.
static DEFAULT_ROUTER: OnceLock<Router> = OnceLock::new();

/// Validate an app name for registration / lookup.
///
/// Constraints:
/// - non-empty after trimming whitespace
/// - at most [`MAX_APP_NAME_LEN`] bytes
/// - ASCII alphanumeric, `_`, or `-` only
///
/// Returns the trimmed name on success.
fn validate_app_name(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("app name must not be empty".to_owned());
    }
    if trimmed.len() > MAX_APP_NAME_LEN {
        return Err(format!(
            "app name too long: {} chars (max {MAX_APP_NAME_LEN})",
            trimmed.len()
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "app name '{trimmed}' contains invalid characters (allowed: alphanumeric, '_', '-')"
        ));
    }
    Ok(trimmed)
}

/// Register the **default** global router factory.
///
/// Equivalent to `register_app_named(DEFAULT_APP_NAME, factory)`.
/// Wire requests without an `"app"` header (or with `"app": ""`) are
/// routed here.
///
/// Any FFI boundary (JNI, C, WASM) calls this once at init time, then
/// uses [`dispatch_from_bytes`] on each request.
///
/// # Second-call semantics
///
/// Calling `register_app` more than once is a **no-op** — the first
/// registration wins, the new factory closure is NOT invoked.  Friendly
/// for environments that legitimately load the cdylib twice (hot-reloading
/// JVM hosts, plugin systems).
pub fn register_app<F>(factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    register_app_named(DEFAULT_APP_NAME, factory);
}

/// Register a **named** global router factory for multi-app routing.
///
/// Wire requests carrying `"app": "<name>"` in their header are
/// dispatched to this router.  Multiple named apps can coexist in
/// the same process; register each once at init time.
///
/// # First-wins per name
///
/// Calling this more than once with the same `name` is a no-op — the
/// first registration wins.  Registering different names is the
/// supported multi-app pattern.
///
/// # Panic safety
///
/// The `factory` closure is invoked **outside** the internal
/// `RwLock`'s write guard.  A panic in `factory` cannot poison the
/// map; the registration is simply discarded and the slot remains
/// available for retry.
///
/// # Invalid names
///
/// Names that fail [`validate_app_name`] (empty, > 64 bytes, or
/// containing characters outside `[A-Za-z0-9_-]`) are silently
/// discarded — registration is a no-op.  Dispatch with a matching
/// invalid name will return a `400` wire response.
pub fn register_app_named<F>(name: &str, factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    let name = match validate_app_name(name) {
        Ok(n) => n.to_owned(),
        Err(_) => return,
    };
    // Fast path: existence check under a read lock.
    {
        let map = APP_ROUTERS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.contains_key(&name) {
            return;
        }
    }
    // Build the router OUTSIDE the write lock so a panicking factory
    // cannot poison the map.
    let router = factory();
    let is_default = name == DEFAULT_APP_NAME;
    let mut map = APP_ROUTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Double-check: another thread may have inserted between our read
    // and write.  First-wins still holds — use Entry to avoid the
    // map.contains_key + map.insert double lookup.
    let stored = map.entry(name).or_insert(router);
    if is_default {
        // Mirror the default app into the lock-free fast path.  Done
        // under the write lock with the *stored* router (not our local
        // candidate) so the mirror always equals the map's first-wins
        // winner, even when two threads race the registration.
        let _ = DEFAULT_ROUTER.set(stored.clone());
    }
}

/// Resolve a [`Router`] for a wire request, applying default-app
/// fallback and name validation.  Returns the cloned router (cheap —
/// axum's router is `Arc`-backed) on success, or a wire error response
/// (`400` for invalid name, `404` for unregistered name) on failure.
///
/// Lookup-first: registered names are validated at registration time
/// ([`register_app_named`] discards invalid names), so a map hit is
/// valid by construction.  Validation runs only on a miss, purely to
/// pick the right error status (`400` invalid vs `404` unregistered)
/// — keeping the per-request hot path to trim + hash lookup.
#[inline]
pub fn resolve_app_router(header: &WireRequestHeader) -> Result<Router, Vec<u8>> {
    let name = header
        .app
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_APP_NAME);
    // Lock-free fast path: default-app dispatch (the common case)
    // resolves with one atomic load — no RwLock acquisition.
    if name == DEFAULT_APP_NAME
        && let Some(router) = DEFAULT_ROUTER.get()
    {
        return Ok(router.clone());
    }
    {
        let map = APP_ROUTERS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(router) = map.get(name) {
            return Ok(router.clone());
        }
    }
    // Miss: decide between 400 (invalid name) and 404 (unregistered).
    match validate_app_name(name) {
        Err(msg) => Err(error_wire(400, &format!("invalid app name: {msg}"))),
        Ok(name) => Err(error_wire(
            404,
            &format!(
                "no app registered with name '{name}' — \
                 use register_app() for the default app or \
                 register_app_named(name, factory) for additional apps"
            ),
        )),
    }
}
