use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use quote::quote;
use serde::{Deserialize, Serialize};

use crate::{
    metadata::{CollectedMetadata, StructMetadata},
    router_codegen::ProcessedVesperaInput,
};

use super::path_utils::{current_crate_tag, find_target_dir};

/// Current cache format. Bump when the on-disk layout changes —
/// old caches deserialize with `cache_format: 0` (serde default) and
/// are treated as a miss.
pub(super) const CACHE_FORMAT: u32 = 1;

/// Cache for avoiding redundant route scanning and OpenAPI generation.
/// Persisted to `target/vespera/routes.cache` across builds.
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
    /// In-repo macro source fingerprint — invalidates cache when the
    /// macro source itself changes during vespera development (the
    /// version alone only changes per release).  `0` for downstream
    /// users.  See [`compute_macro_dev_fingerprint`].
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
}

/// Deterministic content hash for sidecar spec validation.
pub(super) fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone)]
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

    pub(super) fn read(&mut self, path: &Path) -> MergeSpecRead {
        if let Some(cached) = self.reads.get(path) {
            return cached.clone();
        }
        let read = match std::fs::read_to_string(path) {
            Ok(content) => MergeSpecRead::Present(content),
            Err(err) => MergeSpecRead::Error(err.to_string()),
        };
        self.reads.insert(path.to_path_buf(), read.clone());
        read
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
    processed.title.hash(&mut hasher);
    processed.version.hash(&mut hasher);
    processed.docs_url.hash(&mut hasher);
    processed.redoc_url.hash(&mut hasher);
    processed.openapi_file_names.hash(&mut hasher);
    match &processed.servers {
        None => "servers:none".hash(&mut hasher),
        Some(servers) => {
            "servers:some".hash(&mut hasher);
            servers.len().hash(&mut hasher);
            for s in servers {
                s.url.hash(&mut hasher);
                s.description.hash(&mut hasher);
            }
        }
    }
    if let Some(ref schemes) = processed.security_schemes {
        for (name, scheme) in schemes {
            name.hash(&mut hasher);
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
            match serde_json::to_string(scheme) {
                Ok(json) => json.hash(&mut hasher),
                Err(_) => "scheme:unserializable".hash(&mut hasher),
            }
        }
    }
    match &processed.security {
        None => "security:none".hash(&mut hasher),
        Some(security) => {
            "security:some".hash(&mut hasher);
            security.len().hash(&mut hasher);
            for requirement in security {
                let mut names: Vec<_> = requirement.keys().collect();
                names.sort_unstable();
                for name in names {
                    name.hash(&mut hasher);
                }
            }
        }
    }
    if let Some(ref descriptions) = processed.tag_descriptions {
        let mut names: Vec<_> = descriptions.keys().collect();
        names.sort_unstable();
        for name in names {
            name.hash(&mut hasher);
            descriptions[name].hash(&mut hasher);
        }
    }
    // Merge children: hash each child app's NAME *and* its exported
    // OpenAPI sidecar content, so a change to a child's spec invalidates
    // the parent's cached merged document — the path name alone cannot
    // detect a child whose routes / schemas changed between builds.
    // Mirrors the sidecar resolution in `generate_and_write_openapi`
    // (`vespera_dir / <LastSegment>.openapi.json`).
    for merge_path in &processed.merge {
        quote!(#merge_path).to_string().hash(&mut hasher);
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

/// Compute a deterministic hash for `export_app!` inputs.
pub(super) fn compute_export_config_hash(app_name: &str, folder_name: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "export_app:v1".hash(&mut hasher);
    app_name.hash(&mut hasher);
    folder_name.hash(&mut hasher);
    hasher.finish()
}

/// Directory holding child apps' exported OpenAPI sidecars
/// (`<AppName>.openapi.json`), used by [`compute_config_hash`] to fold a
/// merged child's spec content into the parent cache key.  Mirrors the
/// resolution `generate_and_write_openapi` uses when merging child specs.
fn merge_spec_dir() -> Option<std::path::PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    Some(find_target_dir(Path::new(&manifest_dir)).join("vespera"))
}

/// Get the path to this crate's routes cache file.
pub(super) fn get_cache_path() -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_path = Path::new(&manifest_dir);
    find_target_dir(manifest_path)
        .join("vespera")
        .join(format!("routes-{}.cache", current_crate_tag()))
}

/// Get the path to this crate/app/folder's `export_app!` route cache file.
pub(super) fn get_export_cache_path(app_name: &str, folder_name: &str) -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_path = Path::new(&manifest_dir);
    find_target_dir(manifest_path).join("vespera").join(format!(
        "export-routes-{}-{}-{:016x}.cache",
        current_crate_tag(),
        app_name,
        compute_export_config_hash(app_name, folder_name)
    ))
}

/// Fingerprint of the vespera_macro **source tree itself**, for cache
/// invalidation while developing the macro in this repository.
///
/// `macro_version` only changes per release, so editing macro code
/// in-repo would otherwise keep serving the previous build's cached
/// spec.  When `{workspace_root}/crates/vespera_macro/src` exists
/// (i.e. the consuming crate lives inside the vespera repo), hash
/// every `.rs` mtime in it; for downstream users the directory is
/// absent and this is a single failed `stat` (returns 0).
pub(super) fn compute_macro_dev_fingerprint() -> u64 {
    // Memoized per proc-macro process: macro source mtimes cannot change
    // the dll that is currently executing, so one scan per process is
    // exactly as precise as one scan per invocation.  (A fresh cargo
    // build of vespera_macro loads a fresh dll → fresh process state.)
    static MEMO: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MEMO.get_or_init(compute_macro_dev_fingerprint_uncached)
}

fn compute_macro_dev_fingerprint_uncached() -> u64 {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let target_dir = find_target_dir(Path::new(&manifest_dir));
    let Some(workspace_root) = target_dir.parent() else {
        return 0;
    };
    let macro_src = workspace_root
        .join("crates")
        .join("vespera_macro")
        .join("src");
    if !macro_src.is_dir() {
        return 0;
    }
    let mut entries: Vec<(String, u64)> = Vec::new();
    collect_rs_mtimes(&macro_src, &mut entries);
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, mtime) in &entries {
        path.hash(&mut hasher);
        mtime.hash(&mut hasher);
    }
    hasher.finish()
}

/// Recursively collect `(path, mtime)` pairs for `.rs` files.
///
/// Uses `DirEntry::file_type()` / `DirEntry::metadata()` rather than
/// `Path::is_dir()` / `fs::metadata(&path)`: both `DirEntry` accessors
/// are carried by the directory scan (free on Windows + most Unix), so
/// the dir/file split costs no extra `stat` syscall per entry — only
/// the `.rs` files we actually fingerprint pay for their mtime.
fn collect_rs_mtimes(dir: &Path, out: &mut Vec<(String, u64)>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_rs_mtimes(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            // Nanosecond resolution (matching `file_utils::mtime_fingerprint`):
            // whole-second granularity let two edits to the same file within one
            // wall-clock second collide on the same fingerprint, so the route
            // cache could serve a stale router / OpenAPI spec under fast
            // incremental rebuilds. Truncating the u128 nanos-since-epoch to u64
            // keeps every sub-second bit (only overflows past ~year 2554) and the
            // fingerprint is only ever compared for equality.
            let mtime = entry.metadata().and_then(|m| m.modified()).map_or(0, |t| {
                u64::try_from(
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos(),
                )
                .unwrap_or(u64::MAX)
            });
            out.push((path.display().to_string(), mtime));
        }
    }
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
mod tests {
    use std::collections::BTreeMap;

    use vespera_core::schema::{SecurityScheme, SecuritySchemeType};

    use super::*;

    fn base_processed() -> ProcessedVesperaInput {
        ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        }
    }

    #[test]
    fn test_compute_config_hash_with_servers() {
        // Exercises lines 92-96: servers loop in compute_config_hash
        let processed_no_servers = base_processed();

        let processed_with_servers = ProcessedVesperaInput {
            servers: Some(vec![
                vespera_core::openapi::Server {
                    url: "https://api.example.com".to_string(),
                    description: None,
                    variables: None,
                },
                vespera_core::openapi::Server {
                    url: "http://localhost:3000".to_string(),
                    description: None,
                    variables: None,
                },
            ]),
            ..base_processed()
        };

        let hash_no_servers = compute_config_hash(&processed_no_servers);
        let hash_with_servers = compute_config_hash(&processed_with_servers);

        // Different servers should produce different hashes
        assert_ne!(
            hash_no_servers, hash_with_servers,
            "Servers should affect config hash"
        );
    }

    #[test]
    fn test_compute_config_hash_with_merge() {
        // Exercises lines 97-99: merge loop in compute_config_hash
        let processed_no_merge = base_processed();

        let processed_with_merge = ProcessedVesperaInput {
            merge: vec![syn::parse_quote!(app::TestApp)],
            ..base_processed()
        };

        let hash_no_merge = compute_config_hash(&processed_no_merge);
        let hash_with_merge = compute_config_hash(&processed_with_merge);

        assert_ne!(
            hash_no_merge, hash_with_merge,
            "Merge paths should affect config hash"
        );
    }

    #[test]
    fn test_read_cache_corrupt_file_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("routes.cache");
        std::fs::write(&path, "{not valid json").unwrap();
        assert!(read_cache(&path).is_none(), "corrupt cache must be a miss");
    }

    #[test]
    fn test_read_cache_missing_file_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(read_cache(&dir.path().join("nope.cache")).is_none());
    }

    #[test]
    fn test_old_format_cache_deserializes_with_format_zero() {
        // A pre-sidecar cache (inline spec strings, no cache_format
        // field) must still parse — with cache_format defaulting to 0
        // so the orchestrator's `== CACHE_FORMAT` check misses.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("routes.cache");
        let old_format = serde_json::json!({
            "macro_version": "0.1.0",
            "macro_dev_fingerprint": 1u64,
            "file_fingerprints": {},
            "schema_hash": 2u64,
            "config_hash": 3u64,
            "metadata": { "routes": [], "structs": [] },
            "spec_json": "{\"openapi\":\"3.1.0\"}",
            "spec_pretty": "{\n  \"openapi\": \"3.1.0\"\n}"
        });
        std::fs::write(&path, old_format.to_string()).unwrap();
        let cache = read_cache(&path).expect("old format must still deserialize");
        assert_eq!(cache.cache_format, 0, "missing field defaults to 0");
        assert_ne!(cache.cache_format, CACHE_FORMAT, "format check must miss");
        assert!(cache.spec_json_hash.is_none());
        assert!(cache.spec_pretty_hash.is_none());
    }

    #[test]
    fn test_hash_str_deterministic_and_content_sensitive() {
        assert_eq!(hash_str("abc"), hash_str("abc"));
        assert_ne!(hash_str("abc"), hash_str("abd"));
    }

    #[test]
    fn export_config_hash_is_namespaced_by_app_and_folder() {
        let base = compute_export_config_hash("ThirdApp", "routes");

        assert_ne!(base, compute_export_config_hash("AdminApp", "routes"));
        assert_ne!(base, compute_export_config_hash("ThirdApp", "api"));
    }

    #[test]
    fn security_scheme_field_changes_affect_config_hash() {
        fn scheme(http_scheme: &str) -> SecurityScheme {
            SecurityScheme {
                r#type: SecuritySchemeType::Http,
                description: Some("Auth".to_string()),
                name: None,
                r#in: None,
                scheme: Some(http_scheme.to_string()),
                bearer_format: Some("JWT".to_string()),
                flows: None,
                open_id_connect_url: None,
            }
        }

        let bearer = ProcessedVesperaInput {
            security_schemes: Some(BTreeMap::from([(
                "bearerAuth".to_string(),
                scheme("bearer"),
            )])),
            ..base_processed()
        };
        let basic = ProcessedVesperaInput {
            security_schemes: Some(BTreeMap::from([(
                "bearerAuth".to_string(),
                scheme("basic"),
            )])),
            ..base_processed()
        };

        assert_ne!(compute_config_hash(&bearer), compute_config_hash(&basic));
    }

    #[test]
    fn security_none_and_empty_some_have_distinct_config_hashes() {
        let omitted = base_processed();
        let explicit_empty = ProcessedVesperaInput {
            security: Some(Vec::new()),
            ..base_processed()
        };

        assert_ne!(
            compute_config_hash(&omitted),
            compute_config_hash(&explicit_empty)
        );
    }

    #[test]
    fn server_description_changes_affect_config_hash() {
        let production = ProcessedVesperaInput {
            servers: Some(vec![vespera_core::openapi::Server {
                url: "https://api.example.com".to_string(),
                description: Some("Production".to_string()),
                variables: None,
            }]),
            ..base_processed()
        };
        let staging = ProcessedVesperaInput {
            servers: Some(vec![vespera_core::openapi::Server {
                url: "https://api.example.com".to_string(),
                description: Some("Staging".to_string()),
                variables: None,
            }]),
            ..base_processed()
        };

        assert_ne!(
            compute_config_hash(&production),
            compute_config_hash(&staging)
        );
    }
}
