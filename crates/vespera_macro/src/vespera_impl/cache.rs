use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use quote::quote;
use serde::{Deserialize, Serialize};
use vespera_core::schema::SecurityScheme;

use crate::{
    file_utils::{collect_files_with_mtimes, file_fingerprint},
    metadata::{CollectedMetadata, StructMetadata},
    router_codegen::ProcessedVesperaInput,
};

use super::path_utils::{current_crate_tag, find_target_dir};

/// Current cache format. Bump when the on-disk layout changes —
/// old caches deserialize with `cache_format: 0` (serde default) and
/// are treated as a miss.
pub(super) const CACHE_FORMAT: u32 = 2;

/// Cache for avoiding redundant route scanning and OpenAPI generation.
/// Persisted to `target/vespera/routes-<tag>.cache` across builds (one
/// file per `vespera!` invocation — see `super::invocation`).
///
/// The spec JSON strings themselves live in **sidecar files** (the
/// `include_str!` embed file and the pretty sidecar) — the cache only
/// stores their content hashes.  Embedding them inline as JSON strings
/// doubled the cache size via escaping and dominated warm-rebuild
/// `read_cache` time.
#[derive(Serialize, Deserialize)]
pub(super) struct VesperaCache {
    /// On-disk layout version — see [`CACHE_FORMAT`].
    #[serde(default)]
    pub(super) cache_format: u32,
    /// Macro crate version — invalidates cache when macro code changes
    #[serde(default)]
    pub(super) macro_version: String,
    /// Macro (+ `vespera_core`) source fingerprint — invalidates the cache
    /// when a macro built from mutable sources (in-repo, git or `path`
    /// dependency) changed; the version alone only changes per release.
    /// `0` for a packaged crates.io macro.  See
    /// [`compute_macro_dev_fingerprint`].
    #[serde(default)]
    pub(super) macro_dev_fingerprint: u64,
    /// File path → modification time (secs since UNIX_EPOCH)
    pub(super) file_fingerprints: HashMap<String, u64>,
    /// Hash of SCHEMA_STORAGE contents
    pub(super) schema_hash: u64,
    /// Hash of OpenAPI config (title, version, servers, docs_url, etc.)
    pub(super) config_hash: u64,
    /// Cached route/struct metadata
    pub(super) metadata: CollectedMetadata,
    /// Content hash of the compact spec in the embed sidecar file
    /// (`vespera_spec-<tag>.json`).  `None` if docs disabled.
    #[serde(default)]
    pub(super) spec_json_hash: Option<u64>,
    /// Content hash of the pretty spec in the pretty sidecar file
    /// (`openapi_pretty-<tag>.json`).  `None` if no openapi file configured.
    #[serde(default)]
    pub(super) spec_pretty_hash: Option<u64>,
    /// Metadata fingerprint (mtime + len) of the compact spec sidecar.
    #[serde(default)]
    pub(super) spec_json_fingerprint: Option<u64>,
    /// Metadata fingerprint (mtime + len) of the pretty spec sidecar.
    #[serde(default)]
    pub(super) spec_pretty_fingerprint: Option<u64>,
}

/// Borrowed invalidation inputs recomputed on every macro expansion, compared
/// against a persisted [`VesperaCache`] by [`VesperaCache::is_fresh`].
///
/// Exists so the freshness predicate has exactly ONE definition: `vespera!`
/// and `export_app!` previously spelled out the same clause chain inline, so
/// adding an invalidation input and updating only one site silently served a
/// stale OpenAPI spec / router from the other macro.  Adding a field here
/// fails to compile until every construction site supplies it.
pub(super) struct CacheKey<'a> {
    pub(super) macro_version: &'a str,
    pub(super) macro_dev_fingerprint: u64,
    pub(super) file_fingerprints: &'a HashMap<String, u64>,
    pub(super) schema_hash: u64,
    pub(super) config_hash: u64,
}

impl VesperaCache {
    /// Whether this persisted cache may be reused for `key`.
    ///
    /// Covers the invalidation inputs stored in the cache header;
    /// sidecar-file validation is deliberately left to the caller because the
    /// two macros validate different sidecars (`export_app!` adds
    /// `sidecar_matches(...)` at its call site, `vespera!` goes through
    /// `load_validated_sidecar_specs`).
    pub(super) fn is_fresh(&self, key: &CacheKey<'_>) -> bool {
        self.cache_format == CACHE_FORMAT
            && self.macro_version == key.macro_version
            && self.macro_dev_fingerprint == key.macro_dev_fingerprint
            && &self.file_fingerprints == key.file_fingerprints
            && self.schema_hash == key.schema_hash
            && self.config_hash == key.config_hash
    }
}

/// Cheap metadata fingerprint for sidecar files.
///
/// Delegates to the canonical [`file_fingerprint`] mixing shared with
/// route-file and macro-source fingerprinting — see that function for the
/// rationale behind the nanosecond resolution and the size term.
///
/// `None` when the file is missing OR its mtime is unavailable: a
/// mtime-less fingerprint would compare file sizes alone, and
/// [`sidecar_matches`] treats `None` as "unknown" and falls back to the
/// content hash, which stays correct.
pub(super) fn path_fingerprint(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    meta.modified().ok()?;
    Some(file_fingerprint(&meta))
}

/// Validate a sidecar by cheap metadata first, falling back to content hash when
/// the metadata fingerprint changed.
pub(super) fn sidecar_matches(
    path: &Path,
    expected_hash: Option<u64>,
    expected_fingerprint: Option<u64>,
) -> bool {
    let Some(hash) = expected_hash else {
        return false;
    };
    if expected_fingerprint.is_some_and(|fingerprint| path_fingerprint(path) == Some(fingerprint)) {
        return true;
    }
    std::fs::read_to_string(path).is_ok_and(|content| hash_str(&content) == hash)
}

/// Deterministic content hash for sidecar spec validation.
pub(super) fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

pub enum MergeSpecRead {
    Present(String),
    Error(String),
}

pub struct MergeSpecCache {
    dir: Option<PathBuf>,
    reads: HashMap<PathBuf, MergeSpecRead>,
}

impl MergeSpecCache {
    pub(super) fn new() -> Self {
        Self {
            dir: merge_spec_dir(),
            reads: HashMap::new(),
        }
    }

    pub(super) fn spec_file_for(&self, merge_path: &syn::Path) -> Option<(String, PathBuf)> {
        let dir = self.dir.as_ref()?;
        let struct_name = merge_path.segments.last()?.ident.to_string();
        Some((
            struct_name.clone(),
            dir.join(format!("{struct_name}.openapi.json")),
        ))
    }

    /// Read a child app's exported spec sidecar, memoized per path.
    ///
    /// Returns a **borrow** rather than an owned value: both consumers
    /// (`compute_config_hash_with_merge_cache` and
    /// `generate_and_write_openapi`) only ever hash or parse the content,
    /// so handing out ownership meant cloning the child's full serialized
    /// OpenAPI JSON on the miss path *and* on every subsequent hit —
    /// which is exactly the cost this cache exists to avoid.
    pub(super) fn read(&mut self, path: &Path) -> &MergeSpecRead {
        self.reads.entry(path.to_path_buf()).or_insert_with(|| {
            match std::fs::read_to_string(path) {
                Ok(content) => MergeSpecRead::Present(content),
                Err(err) => MergeSpecRead::Error(err.to_string()),
            }
        })
    }
}

/// Compute a deterministic hash of SCHEMA_STORAGE contents.
pub(super) fn compute_schema_hash(schema_storage: &HashMap<String, StructMetadata>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut keys: Vec<&String> = schema_storage.keys().collect();
    keys.sort();
    for key in keys {
        key.hash(&mut hasher);
        let meta = &schema_storage[key];
        meta.name.hash(&mut hasher);
        meta.definition.hash(&mut hasher);
        meta.include_in_openapi.hash(&mut hasher);
        // Field defaults (`#[serde(default = "fn")]`) feed the generated
        // OpenAPI `default` values but are NOT part of `definition`, so a
        // changed default would otherwise hit a STALE route cache and reuse
        // outdated spec defaults.  `BTreeMap` iterates in sorted key order
        // (deterministic); hash each field name + its serialized JSON value.
        for (field, value) in &meta.field_defaults {
            field.hash(&mut hasher);
            value.to_string().hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Fold the `#[route]` signatures stored for this crate into `schema_hash`.
///
/// `#[route]` expands return-type aliases (`-> ApiResult<User>`) through
/// definitions that may live OUTSIDE the route folder (e.g. `src/error.rs`),
/// which the route-file fingerprints do not cover. Hashing the stored
/// (already expanded) signatures keeps an alias edit from serving a stale
/// spec.
pub(super) fn with_route_signature_hash(
    schema_hash: u64,
    route_storage: &[crate::route_impl::StoredRouteInfo],
) -> u64 {
    let mut signatures: Vec<(&str, &str, &str)> = route_storage
        .iter()
        .map(|route| {
            (
                route.file_path.as_deref().unwrap_or_default(),
                route.fn_name.as_str(),
                route.fn_sig_str.as_str(),
            )
        })
        .collect();
    signatures.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    schema_hash.hash(&mut hasher);
    for signature in &signatures {
        signature.hash(&mut hasher);
    }
    hasher.finish()
}

/// Compute a deterministic hash of OpenAPI config fields.
#[cfg(test)]
pub(super) fn compute_config_hash(processed: &ProcessedVesperaInput) -> u64 {
    let mut merge_specs = MergeSpecCache::new();
    compute_config_hash_with_merge_cache(processed, &mut merge_specs)
}

/// Compute a deterministic hash of OpenAPI config fields, sharing merge-sidecar reads.
pub(super) fn compute_config_hash_with_merge_cache(
    processed: &ProcessedVesperaInput,
    merge_specs: &mut MergeSpecCache,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_invocation_settings(processed, &mut hasher);
    super::path_utils::package_description().hash(&mut hasher);
    // Merge children: fold each child app's exported OpenAPI sidecar
    // content (the names are part of the settings above), so a change to a
    // child's spec invalidates the parent's cached merged document — the
    // path name alone cannot detect a child whose routes / schemas changed
    // between builds.  Mirrors the sidecar resolution in
    // `generate_and_write_openapi` (`vespera_dir / <LastSegment>.openapi.json`).
    for merge_path in &processed.merge {
        if let Some((_struct_name, spec_file)) = merge_specs.spec_file_for(merge_path) {
            match merge_specs.read(&spec_file) {
                MergeSpecRead::Present(content) => content.hash(&mut hasher),
                // Absent / unreadable child sidecar → stable marker so the
                // hashed state still differs from a present spec.
                MergeSpecRead::Error(_) => "child-spec:absent".hash(&mut hasher),
            }
        }
    }
    hasher.finish()
}

/// Hash a `vespera!` invocation's own (normalized) macro settings — every
/// input field, but nothing read from disk or the environment.  Shared by
/// the cache key above and the per-invocation file tag
/// (`super::invocation::invocation_tag`).
pub(super) fn hash_invocation_settings(
    processed: &ProcessedVesperaInput,
    hasher: &mut impl Hasher,
) {
    processed.folder_name.hash(hasher);
    processed.title.hash(hasher);
    processed.version.hash(hasher);
    processed.info.hash(hasher);
    processed.docs_url.hash(hasher);
    processed.redoc_url.hash(hasher);
    processed.spec_url.hash(hasher);
    processed.openapi_file_names.hash(hasher);
    match &processed.servers {
        None => "servers:none".hash(hasher),
        Some(servers) => {
            "servers:some".hash(hasher);
            servers.len().hash(hasher);
            for s in servers {
                s.url.hash(hasher);
                s.description.hash(hasher);
            }
        }
    }
    if let Some(ref schemes) = processed.security_schemes {
        for (name, scheme) in schemes {
            name.hash(hasher);
            // Hash a STABLE serialized representation of the whole scheme
            // rather than a hand-picked field subset.  The previous list
            // omitted `flows` and `open_id_connect_url`, so changing only an
            // OIDC discovery URL hit the warm route cache and reused stale
            // OpenAPI output.  `serde_json` renders struct fields in
            // declaration order (deterministic) and `skip_serializing_if`
            // only drops `None`s, so the digest is faithful AND future-proof:
            // any field added to `SecurityScheme` is covered automatically,
            // closing this class of stale-cache bug for good.  Serialization
            // is infallible for this plain struct; a hypothetical failure
            // falls back to a stable marker so the hash still differs.
            hash_security_scheme(scheme, hasher);
        }
    }
    match &processed.security {
        None => "security:none".hash(hasher),
        Some(security) => {
            "security:some".hash(hasher);
            security.len().hash(hasher);
            // Hash each requirement's scheme names AND scopes (BTreeMap →
            // sorted, deterministic). Scopes alone changing (`oauth2:
            // ["read"]` → `["read", "write"]`) must invalidate the cache.
            for requirement in security {
                requirement.len().hash(hasher);
                for (name, scopes) in requirement {
                    name.hash(hasher);
                    scopes.hash(hasher);
                }
            }
        }
    }
    if let Some(ref descriptions) = processed.tag_descriptions {
        let mut names: Vec<_> = descriptions.keys().collect();
        names.sort_unstable();
        for name in names {
            name.hash(hasher);
            descriptions[name].hash(hasher);
        }
    }
    for merge_path in &processed.merge {
        quote!(#merge_path).to_string().hash(hasher);
    }
}

/// Hashes one complete security scheme, preserving the stable serialization
/// fallback while taking the destination hasher explicitly for loop reuse.
fn hash_security_scheme(scheme: &SecurityScheme, hasher: &mut impl Hasher) {
    serde_json::to_string(scheme)
        .as_deref()
        .unwrap_or("scheme:unserializable")
        .hash(hasher);
}

/// Compute a deterministic hash for `export_app!` inputs.
pub(super) fn compute_export_config_hash(app_name: &str, folder_name: &str, prefix: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "export_app:v1".hash(&mut hasher);
    app_name.hash(&mut hasher);
    folder_name.hash(&mut hasher);
    prefix.hash(&mut hasher);
    hasher.finish()
}

/// Directory holding child apps' exported OpenAPI sidecars
/// (`<AppName>.openapi.json`), used by [`compute_config_hash`] to fold a
/// merged child's spec content into the parent cache key.  Mirrors the
/// resolution `generate_and_write_openapi` uses when merging child specs.
fn merge_spec_dir() -> Option<std::path::PathBuf> {
    let manifest_dir = crate::env_access::var("CARGO_MANIFEST_DIR").ok()?;
    Some(find_target_dir(Path::new(&manifest_dir)).join("vespera"))
}

/// Get the path to a `vespera!` invocation's routes cache file; `tag` is
/// its `super::invocation::invocation_tag`.
pub(super) fn get_cache_path(tag: &str) -> std::path::PathBuf {
    let manifest_dir = crate::env_access::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_path = Path::new(&manifest_dir);
    find_target_dir(manifest_path)
        .join("vespera")
        .join(format!("routes-{tag}.cache"))
}

/// Get the path to this crate/app/folder's `export_app!` route cache file.
pub(super) fn get_export_cache_path(
    app_name: &str,
    folder_name: &str,
    prefix: &str,
) -> std::path::PathBuf {
    let manifest_dir = crate::env_access::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_path = Path::new(&manifest_dir);
    find_target_dir(manifest_path).join("vespera").join(format!(
        "export-routes-{}-{}-{:016x}.cache",
        current_crate_tag(),
        app_name,
        compute_export_config_hash(app_name, folder_name, prefix)
    ))
}

/// Fingerprint of the vespera_macro **source tree itself** (plus the
/// sibling `vespera_core` sources compiled into it), for cache
/// invalidation whenever the macro is built from mutable sources.
///
/// `macro_version` only changes per release, so editing macro code — or
/// the `vespera_core` OpenAPI types the macro serializes — would otherwise
/// keep serving the previous build's cached spec.  The sources are located
/// through the macro crate's **own** compile-time `CARGO_MANIFEST_DIR`, so
/// this covers the in-repo workspace, git dependencies and out-of-repo
/// `path` dependencies alike (the former consumer-workspace probe only
/// covered the first, and never looked at `vespera_core`).  A *packaged*
/// macro (crates.io / vendored — it carries the `Cargo.toml.orig` that
/// `cargo package` adds) is immutable per version: one `stat`, returns 0.
pub(super) fn compute_macro_dev_fingerprint() -> u64 {
    // Memoized per proc-macro process: macro source mtimes cannot change
    // the dll that is currently executing, so one scan per process is
    // exactly as precise as one scan per invocation.  (A fresh cargo
    // build of vespera_macro loads a fresh dll → fresh process state.)
    static MEMO: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MEMO.get_or_init(|| macro_dev_fingerprint_for(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

/// Fingerprint the macro sources of the macro crate at `macro_manifest_dir`
/// (see [`compute_macro_dev_fingerprint`]); the root is explicit so tests can
/// exercise the packaged and source-tree layouts.
fn macro_dev_fingerprint_for(macro_manifest_dir: &Path) -> u64 {
    if macro_manifest_dir.join("Cargo.toml.orig").exists() {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for dir in macro_src_dirs(macro_manifest_dir) {
        fingerprint_rs_directory(&dir).hash(&mut hasher);
    }
    hasher.finish()
}

/// The source directories whose edits change macro output: the macro crate's
/// own `src` and the sibling `vespera_core/src`.  A missing directory
/// fingerprints as 0.
fn macro_src_dirs(macro_manifest_dir: &Path) -> [PathBuf; 2] {
    [
        macro_manifest_dir.join("src"),
        macro_manifest_dir
            .join("..")
            .join("vespera_core")
            .join("src"),
    ]
}

/// Fingerprints only Rust files below the supplied directory, taking the root
/// explicitly so callers and tests share identical ordering and hashing.
fn fingerprint_rs_directory(macro_src: &Path) -> u64 {
    collect_files_with_mtimes(macro_src).map_or(0, hash_rs_fingerprints)
}

/// Order-independent fingerprint over the `.rs` entries of a completed scan.
///
/// The fingerprint is the canonical mtime+size [`file_fingerprint`] the scan
/// already computed, so a timestamp-preserving edit to macro source (git
/// checkout, `cp -p`, build-cache restore) invalidates downstream route caches
/// instead of silently serving a stale spec.
fn hash_rs_fingerprints(files: Vec<(PathBuf, u64)>) -> u64 {
    let mut entries: Vec<(String, u64)> = files
        .into_iter()
        .filter(|(path, _)| path.extension().is_some_and(|e| e == "rs"))
        .map(|(path, fingerprint)| (path.display().to_string(), fingerprint))
        .collect();
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, mtime) in &entries {
        path.hash(&mut hasher);
        mtime.hash(&mut hasher);
    }
    hasher.finish()
}

/// Try to read and deserialize a cache file. Returns None on any failure.
pub(super) fn read_cache(cache_path: &Path) -> Option<VesperaCache> {
    let content = std::fs::read_to_string(cache_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write cache to disk. Failures are silently ignored (cache is best-effort).
pub(super) fn write_cache(cache_path: &Path, cache: &VesperaCache) {
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(cache_path, json);
    }
}

#[cfg(test)]
mod tests;
