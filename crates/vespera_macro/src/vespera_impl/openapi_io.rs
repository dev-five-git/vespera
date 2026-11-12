use std::{collections::HashMap, path::Path};

use crate::{
    error::{MacroResult, err_call_site},
    metadata::CollectedMetadata,
    openapi_generator::{
        OpenApiSecurity, find_duplicate_operation_id, try_generate_openapi_doc_with_metadata,
        validate_security_references,
    },
    route_impl::StoredRouteInfo,
    router_codegen::ProcessedVesperaInput,
};
use proc_macro2::Span;
use syn::spanned::Spanned;

use super::{
    cache::{MergeSpecCache, MergeSpecRead, path_fingerprint},
    path_utils::{find_target_dir, resolve_manifest_relative},
    schema_merge::SchemaMergeGuard,
};

fn display_merge_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// OpenAPI write result consumed by router/doc codegen and incremental cache sidecars.
///
/// The docs/redoc URLs are intentionally **not** carried here: the sole
/// consumer (`vespera_impl::orchestrator`) reads them straight off
/// `ProcessedVesperaInput`, so mirroring them would be dead weight.
#[derive(Debug)]
pub struct OpenApiWriteResult {
    pub spec_json: Option<String>,
    pub spec_pretty: Option<String>,
}

pub(super) struct EmbeddedSpecWrite {
    pub(super) tokens: proc_macro2::TokenStream,
    pub(super) fingerprint: Option<u64>,
}

/// Whether `path` already holds exactly `content`.
///
/// A cheap `metadata().len()` pre-check skips the full `read_to_string`
/// whenever the byte length alone proves the content changed (the common
/// case when a regenerated spec differs) — only an exact length match
/// falls back to the full read + compare.  Missing or unreadable files
/// count as "changed", so the caller writes — exactly like the previous
/// `read_to_string(...).map_or(true, |e| e != content)` this replaces.
pub(super) fn content_unchanged(path: &Path, content: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == content.len() as u64)
        && std::fs::read_to_string(path).is_ok_and(|existing| existing == content)
}

pub(super) fn write_if_changed(path: &Path, content: &str) -> std::io::Result<Option<u64>> {
    if !content_unchanged(path, content) {
        std::fs::write(path, content)?;
    }
    Ok(path_fingerprint(path))
}

/// Whether the router serves the spec at runtime (docs pages or `spec_url`),
/// which requires embedding the compact JSON.
fn serves_spec(input: &ProcessedVesperaInput) -> bool {
    input.docs_url.is_some() || input.redoc_url.is_some() || input.spec_url.is_some()
}

/// Generate `OpenAPI` JSON and write to files, returning docs info
pub fn generate_and_write_openapi(
    input: &ProcessedVesperaInput,
    metadata: &CollectedMetadata,
    file_asts: HashMap<String, syn::File>,
    route_storage: &[StoredRouteInfo],
    merge_specs: &mut MergeSpecCache,
) -> MacroResult<OpenApiWriteResult> {
    if input.openapi_file_names.is_empty() && !serves_spec(input) {
        return Ok(OpenApiWriteResult {
            spec_json: None,
            spec_pretty: None,
        });
    }

    let mut openapi_doc = try_generate_openapi_doc_with_metadata(
        input.title.clone(),
        input.version.clone(),
        input.servers.clone(),
        Some(OpenApiSecurity {
            security_schemes: input.security_schemes.clone(),
            security: input.security.clone(),
            tag_descriptions: input.tag_descriptions.clone(),
        }),
        metadata,
        Some(file_asts),
        route_storage,
    )?;
    input.info.apply(&mut openapi_doc.info);
    if openapi_doc.info.description.is_none() {
        openapi_doc.info.description = super::path_utils::package_description();
    }

    // Merge specs from child apps at compile time. This is the one point where
    // the parent and every exported child definition are all available, so
    // schema-name conflicts are checked here before OpenApi's first-wins merge
    // can discard a later definition.
    if !input.merge.is_empty() {
        let mut schema_guard = SchemaMergeGuard::new(&openapi_doc)?;
        for merge_path in &input.merge {
            // Extract the struct name (last segment, e.g., "ThirdApp" from "third::ThirdApp")
            if let Some((struct_name, spec_file)) = merge_specs.spec_file_for(merge_path) {
                let spec_content = match merge_specs.read(&spec_file) {
                    MergeSpecRead::Present(content) => content,
                    MergeSpecRead::Error(e) => {
                        return Err(err_call_site(format!(
                            "OpenAPI merge: failed to read child spec for `{struct_name}` at '{}'. Error: {e}. Ensure the child crate containing `export_app!({struct_name})` is built before the parent app.",
                            spec_file.display()
                        )));
                    }
                };
                let child_spec = serde_json::from_str::<vespera_core::openapi::OpenApi>(spec_content).map_err(|e| err_call_site(format!("OpenAPI merge: failed to parse child spec for `{struct_name}` at '{}'. Error: {e}.", spec_file.display())))?;
                let child_origin = format!("merged app `{}`", display_merge_path(merge_path));
                schema_guard.check_child(&child_spec, &child_origin, merge_path.span())?;
                openapi_doc.merge(child_spec);
                // The parent's own operationIds are already unique; any clash
                // now was introduced by this child.
                if let Some((operation_id, first, second)) =
                    find_duplicate_operation_id(&openapi_doc.paths)
                {
                    return Err(syn::Error::new(
                        merge_path.span(),
                        format!(
                            "OpenAPI merge: {child_origin} introduces duplicate operationId \
                             `{operation_id}` (used by both `{first}` and `{second}`) — \
                             operationIds must be unique; rename one with \
                             `#[vespera::route(..., operation_id = \"...\")]`"
                        ),
                    ));
                }
            }
        }
    }

    // Security-scheme references are validated only AFTER the child specs are
    // merged: a parent route may legitimately reference a scheme that only a
    // merged child declares.  Enforced only when this app declares
    // `security_schemes` itself (same opt-in as before).
    if input.security_schemes.is_some() {
        let empty = std::collections::BTreeMap::new();
        let merged_schemes = openapi_doc
            .components
            .as_ref()
            .and_then(|components| components.security_schemes.as_ref())
            .unwrap_or(&empty);
        validate_security_references(metadata, merged_schemes, input.security.as_deref())?;
    }

    // NOTE on F-01: an earlier audit suggested serialising the
    // `OpenApi` document once into `serde_json::Value` and emitting
    // pretty + compact from the cached `Value`.  We deliberately do
    // **not** do that here.  Going through `Value` re-orders every
    // object's keys alphabetically (because the default
    // `serde_json::Map` is `BTreeMap`-backed), which silently changes
    // the field order in every user-visible `openapi.json` file.  The
    // marginal build-time saving is not worth churning the output of a
    // file users diff in CI.  Keep two direct serialisations.
    //
    // Pretty-print for user-visible files.
    let spec_pretty = if input.openapi_file_names.is_empty() {
        None
    } else {
        let json_pretty = serde_json::to_string_pretty(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?;
        for openapi_file_name in &input.openapi_file_names {
            let file_path = &resolve_manifest_relative(openapi_file_name);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| err_call_site(format!("OpenAPI output: failed to create directory '{}'. Error: {}. Ensure the path is valid and writable.", parent.display(), e)))?;
            }
            let should_write = !content_unchanged(file_path, &json_pretty);
            if should_write {
                std::fs::write(file_path, &json_pretty).map_err(|e| err_call_site(format!("OpenAPI output: failed to write file '{openapi_file_name}'. Error: {e}. Ensure the file path is writable.")))?;
            }
        }
        Some(json_pretty)
    };

    // Compact JSON for embedding (smaller binary, faster downstream compilation).
    let spec_json = if serves_spec(input) {
        Some(serde_json::to_string(&openapi_doc).map_err(|e| err_call_site(format!("OpenAPI generation: failed to serialize document to JSON. Error: {e}. Check that all schema types are serializable.")))?)
    } else {
        None
    };

    Ok(OpenApiWriteResult {
        spec_json,
        spec_pretty,
    })
}

/// Write cached OpenAPI spec to output files if they are stale or missing.
pub fn ensure_openapi_files_from_cache(
    openapi_file_names: &[String],
    spec_pretty: Option<&str>,
) -> syn::Result<()> {
    let Some(pretty) = spec_pretty else {
        return Ok(());
    };
    for openapi_file_name in openapi_file_names {
        let file_path = &resolve_manifest_relative(openapi_file_name);
        let should_write = !content_unchanged(file_path, pretty);
        if should_write {
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    syn::Error::new(
                        Span::call_site(),
                        format!(
                            "OpenAPI output: failed to create directory '{}': {}",
                            parent.display(),
                            e
                        ),
                    )
                })?;
            }
            std::fs::write(file_path, pretty).map_err(|e| {
                syn::Error::new(
                    Span::call_site(),
                    format!("OpenAPI output: failed to write file '{openapi_file_name}': {e}"),
                )
            })?;
        }
    }
    Ok(())
}

/// Path of the compact-spec embed sidecar (`include_str!` target).
///
/// The file name is **namespaced per invocation** (`tag` =
/// `super::invocation::invocation_tag`: crate name + settings hash): two
/// workspace members both using `vespera!` compile in parallel under the
/// same shared `target/vespera/` directory, and one crate may hold several
/// `vespera!` calls with different settings — with a shared file name, one
/// invocation's `include_str!` could read the spec another just wrote.
pub(super) fn embed_spec_path(tag: &str) -> std::path::PathBuf {
    let manifest_dir = crate::env_access::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    find_target_dir(Path::new(&manifest_dir))
        .join("vespera")
        .join(format!("vespera_spec-{tag}.json"))
}

/// Path of the pretty-spec sidecar (warm-rebuild source for
/// `openapi.json` recovery — see `ensure_openapi_files_from_cache`).
pub(super) fn pretty_sidecar_path(tag: &str) -> std::path::PathBuf {
    let manifest_dir = crate::env_access::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    find_target_dir(Path::new(&manifest_dir))
        .join("vespera")
        .join(format!("openapi_pretty-{tag}.json"))
}

/// Build the `include_str!` tokens pointing at the embed sidecar.
fn embed_tokens(spec_file: &Path) -> proc_macro2::TokenStream {
    let path_str = crate::file_utils::path_to_include_str_literal(spec_file);
    quote::quote! { include_str!(#path_str) }
}

/// Hash-validated sidecar specs loaded on a warm cache hit.
pub(super) struct SidecarSpecs {
    /// Pretty spec content (for `openapi.json` recovery); `None` when
    /// no openapi file is configured.
    pub(super) pretty: Option<String>,
    /// `include_str!` tokens for the embed sidecar; `None` when docs
    /// are disabled.
    pub(super) spec_tokens: Option<proc_macro2::TokenStream>,
}

/// Load and hash-validate the sidecar spec files on a warm cache hit.
///
/// Returns `None` when any expected sidecar is missing or fails its
/// content-hash check — the caller must then treat the cache as a miss
/// (a full regeneration rewrites both sidecars, so corruption
/// self-heals on the next build).
pub(super) fn load_validated_sidecar_specs(
    tag: &str,
    spec_json_hash: Option<u64>,
    spec_pretty_hash: Option<u64>,
    spec_json_fingerprint: Option<u64>,
    spec_pretty_fingerprint: Option<u64>,
) -> Option<SidecarSpecs> {
    let spec_tokens = match (spec_json_hash, spec_json_fingerprint) {
        (None, _) => None,
        (Some(expected_hash), Some(expected_fingerprint)) => {
            let path = embed_spec_path(tag);
            if !super::cache::sidecar_matches(
                &path,
                Some(expected_hash),
                Some(expected_fingerprint),
            ) {
                return None;
            }
            Some(embed_tokens(&path))
        }
        (Some(_), None) => return None,
    };
    let pretty = match (spec_pretty_hash, spec_pretty_fingerprint) {
        (None, _) => None,
        (Some(expected_hash), Some(expected_fingerprint)) => {
            let path = pretty_sidecar_path(tag);
            let content = std::fs::read_to_string(&path).ok()?;
            if super::cache::path_fingerprint(&path) != Some(expected_fingerprint)
                && super::cache::hash_str(&content) != expected_hash
            {
                return None;
            }
            Some(content)
        }
        (Some(_), None) => return None,
    };
    Some(SidecarSpecs {
        pretty,
        spec_tokens,
    })
}

/// Write the pretty-spec sidecar (write-if-differs).  Best-effort like
/// the cache itself: failures only cost a future cache miss.
pub(super) fn write_pretty_sidecar(tag: &str, spec_pretty: Option<&str>) -> Option<u64> {
    let pretty = spec_pretty?;
    let path = pretty_sidecar_path(tag);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    write_if_changed(&path, pretty).ok().flatten()
}

/// Write compact spec JSON to target dir for `include_str!` embedding.
pub(super) fn write_spec_for_embedding(
    tag: &str,
    spec_json: Option<String>,
) -> syn::Result<Option<EmbeddedSpecWrite>> {
    let Some(json) = spec_json else {
        return Ok(None);
    };
    let spec_file = embed_spec_path(tag);
    if let Some(parent) = spec_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            syn::Error::new(
                Span::call_site(),
                format!(
                    "vespera! macro: failed to create directory '{}': {}",
                    parent.display(),
                    e
                ),
            )
        })?;
    }
    let fingerprint = write_if_changed(&spec_file, &json).map_err(|e| {
        syn::Error::new(
            Span::call_site(),
            format!(
                "vespera! macro: failed to write spec file '{}': {}",
                spec_file.display(),
                e
            ),
        )
    })?;
    Ok(Some(EmbeddedSpecWrite {
        tokens: embed_tokens(&spec_file),
        fingerprint,
    }))
}

#[cfg(test)]
mod tests;
