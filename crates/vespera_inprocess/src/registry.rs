//! App registry: named `Router` factories with a lock-free
//! `OnceLock` fast path for the default app.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};

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
/// through `APP_ROUTERS` still costs an `ArcSwap` load + hash lookup
/// per request.  This `OnceLock` mirror is set (exactly once, by the
/// first successful `_default` registration so it can never diverge
/// from the map) and read with a single atomic load + `Router::clone`
/// (`Arc` refcount bump) on every dispatch — skipping even the hash
/// lookup.
///
/// Named apps resolve through the lock-free [`ArcSwap`] load — they are
/// the rare multi-app case and can be registered at any time.
static DEFAULT_ROUTER: OnceLock<Router> = OnceLock::new();

/// Serializes the registration **write path** (`register_app*`) so a given
/// app name's `factory` runs **at most once**, even under concurrent
/// same-name registration: without it, two racing registrations both pass
/// the `contains_key` pre-check and each invoke their `factory` (the loser's
/// router is then discarded by the first-wins insert) — observable when a
/// factory has side effects or is expensive.  Dispatch is unaffected: the
/// read path ([`resolve_app_router`]) never touches this lock and stays
/// fully lock-free.
static REGISTER_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// Set while a [`try_register_app_named`] call on this thread is running
    /// its `factory` closure.  A re-entrant `register_app*` call from inside a
    /// factory would otherwise deadlock the non-reentrant [`REGISTER_LOCK`];
    /// the flag lets the re-entrant call be rejected with an error instead.
    static REGISTERING: Cell<bool> = const { Cell::new(false) };
}

/// RAII reset for the [`REGISTERING`] thread-local flag: clears it on EVERY
/// exit path of the guarded `factory()` call — including a panic unwinding out
/// of the factory — so a panicking factory never wedges the thread into the
/// permanent "re-entrant" state where every future registration fails.
struct ReentryGuard;

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        REGISTERING.with(|r| r.set(false));
    }
}

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
            "app name too long: {} bytes (max {MAX_APP_NAME_LEN})",
            trimmed.len()
        ));
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
/// The `factory` closure is invoked **outside** the [`ArcSwap`]
/// copy-on-write update.  A panic in `factory` cannot corrupt the
/// registry; the registration is simply discarded and the slot remains
/// available for retry.
///
/// # Invalid names
///
/// Names that fail [`validate_app_name`] (empty, > 64 bytes, or
/// containing characters outside `[A-Za-z0-9_-]`) are silently
/// discarded — registration is a no-op.  Dispatch with a matching
/// invalid name will return a `400` wire response.  Use
/// [`try_register_app_named`] to surface an invalid name (or an
/// already-registered one) as a `Result` instead of a silent no-op.
pub fn register_app_named<F>(name: &str, factory: F)
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    // BC sugar over the fallible form: an invalid or already-registered name
    // is silently a no-op. Hosts that need to detect those outcomes call
    // [`try_register_app_named`] directly.
    let _ = try_register_app_named(name, factory);
}

/// Fallible sibling of [`register_app_named`] that **reports the outcome**
/// instead of silently swallowing it:
///
/// - `Ok(true)`  — newly registered (the factory ran and the router was stored)
/// - `Ok(false)` — a router was already registered under this name; first-wins,
///   so the factory was NOT invoked
/// - `Err(msg)`  — `name` failed [`validate_app_name`] (empty, > 64 bytes, or
///   characters outside `[A-Za-z0-9_-]`); nothing was registered
///
/// A multi-app host can surface a typo'd app name at startup — instead of
/// discovering it only when every dispatch to that app silently returns
/// `404` / `400`.
///
/// First-wins semantics, lock-free dispatch reads, and factory panic safety
/// are identical to [`register_app_named`].
///
/// # Re-entrancy
///
/// `factory` runs while the registration write-path mutex ([`REGISTER_LOCK`])
/// is held, so a given name's factory runs **at most once** even under a
/// concurrent same-name race.  It therefore MUST NOT call back into
/// `register_app*` from within itself — doing so re-enters the non-reentrant
/// lock and deadlocks.  Registration is a startup-time operation: build the
/// `Router` inside `factory` without registering further apps from within it.
pub fn try_register_app_named<F>(name: &str, factory: F) -> Result<bool, String>
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    let name = validate_app_name(name)?.to_owned();
    // Re-entrancy guard: `factory` runs while [`REGISTER_LOCK`] is held (so a
    // name's factory runs at most once under a concurrent same-name race).
    // `std::sync::Mutex` is non-reentrant, so a factory that calls back into
    // `register_app*` on the SAME thread would deadlock process startup.
    // Detect that re-entrancy BEFORE taking the lock and return an error
    // instead of hanging — the documented contract is now enforced, not just
    // warned about.
    if REGISTERING.with(Cell::get) {
        return Err("re-entrant app registration: a router factory must not \
                    register apps from within itself"
            .to_owned());
    }
    // Serialize the registration write path (dispatch reads stay lock-free)
    // so a given name's `factory` runs at most once — see [`REGISTER_LOCK`].
    let _guard = REGISTER_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    // Re-check under the lock: first-wins, so an already-present name means
    // `factory` is NOT invoked.
    if APP_ROUTERS.load().contains_key(&name) {
        return Ok(false);
    }
    // Build the router OUTSIDE the copy-on-write update so a panicking
    // factory cannot corrupt the registry: the panic propagates before any
    // insert, leaving the registry untouched (the poisoned lock is recovered
    // by the next registration).  The re-entrancy flag is set only around the
    // `factory()` call and cleared by `ReentryGuard::drop` on every exit path
    // (incl. a factory panic), so a re-entrant `register_app*` from inside the
    // factory is rejected with an error rather than deadlocking.
    let router = {
        REGISTERING.with(|r| r.set(true));
        let _reentry = ReentryGuard;
        factory()
    };
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
    Ok(true)
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
