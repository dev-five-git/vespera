//! App registry: named `Router` factories with a lock-free
//! `OnceLock` fast path for the default app.

use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock};

use arc_swap::ArcSwap;

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
/// Backed by [`ArcSwap`] so dispatch **reads are lock-free** — a named
/// app resolves with a single atomic load + hash lookup, no lock
/// acquisition and no reader parking under high concurrency (the same
/// quality the default app already gets from its [`OnceLock`] mirror).
///
/// The map is append-only with first-wins semantics and is written only
/// during `register_app*` calls (typically at process startup).  Writes
/// go through copy-on-write [`ArcSwap::rcu`]: clone the (small) map,
/// `entry().or_insert` the new router, and atomically publish the new
/// snapshot.  Factory closures are invoked **outside** the update, so a
/// factory panic cannot corrupt the registry; there is no lock to
/// poison.
static APP_ROUTERS: LazyLock<ArcSwap<HashMap<String, Router>>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

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
    // Fast path: already registered? Lock-free load + lookup.
    if APP_ROUTERS.load().contains_key(&name) {
        return;
    }
    // Build the router OUTSIDE the copy-on-write update so a panicking
    // factory cannot corrupt the registry; built once even if `rcu`
    // retries under concurrent registration (it only re-clones the map
    // and re-applies the same first-wins insert with this `router`).
    let router = factory();
    let is_default = name == DEFAULT_APP_NAME;
    APP_ROUTERS.rcu(|current| {
        let mut next: HashMap<String, Router> = (**current).clone();
        // First-wins: `or_insert_with` leaves an existing entry (from a
        // racing registration) untouched, so the first inserter wins.
        next.entry(name.clone()).or_insert_with(|| router.clone());
        next
    });
    if is_default {
        // Mirror the first-wins default winner into the lock-free
        // OnceLock fast path.  The map is append-only, so the
        // `_default` entry is stable once present.
        if let Some(stored) = APP_ROUTERS.load().get(DEFAULT_APP_NAME) {
            let _ = DEFAULT_ROUTER.set(stored.clone());
        }
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
    // resolves with one atomic load — no lock acquisition.
    if name == DEFAULT_APP_NAME
        && let Some(router) = DEFAULT_ROUTER.get()
    {
        return Ok(router.clone());
    }
    // Named-app resolution is also lock-free: a single `ArcSwap` load
    // (atomic) + hash lookup, no reader parking under concurrency.
    if let Some(router) = APP_ROUTERS.load().get(name) {
        return Ok(router.clone());
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
