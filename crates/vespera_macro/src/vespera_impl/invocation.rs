//! Per-invocation namespacing of `vespera!`'s build files.
//!
//! A crate may call `vespera!` more than once with DIFFERENT settings (e.g.
//! a default app and an admin app with their own `dir` / `title` /
//! `openapi`).  Every invocation persists a route cache plus two spec
//! sidecars under `target/vespera/`; when those were keyed by crate name
//! alone, the invocations overwrote each other's files on every build — the
//! cache never hit (each expansion found the other invocation's config) and
//! the generated `include_str!` of the shared embed sidecar only picked up
//! the right spec because rustc happens to expand it before the next
//! `vespera!` rewrites the file.
//!
//! The tag mixes the crate name with a hash of the invocation's own
//! normalized settings (`cache::hash_invocation_settings`), so distinct
//! invocations get distinct files while identical invocations (e.g. the same
//! router built with and without a layer) still share one.

use std::hash::{Hash, Hasher};

use crate::router_codegen::ProcessedVesperaInput;

use super::{cache::hash_invocation_settings, path_utils::current_crate_tag};

/// File-name tag (`<crate>-<16 hex>`) for one `vespera!` invocation's
/// route cache (`routes-<tag>.cache`) and spec sidecars
/// (`vespera_spec-<tag>.json`, `openapi_pretty-<tag>.json`).
///
/// Depends only on the macro input — never on disk contents (merged child
/// specs) or the environment (`CARGO_PKG_DESCRIPTION`), which the cache key
/// already covers — so a file keeps its name across builds as long as the
/// invocation itself is unchanged.
pub(super) fn invocation_tag(processed: &ProcessedVesperaInput) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "vespera:invocation:v1".hash(&mut hasher);
    hash_invocation_settings(processed, &mut hasher);
    format!("{}-{:016x}", current_crate_tag(), hasher.finish())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::env_access::test_env;

    fn base() -> ProcessedVesperaInput {
        ProcessedVesperaInput {
            folder_name: "routes".to_string(),
            ..ProcessedVesperaInput::default()
        }
    }

    #[test]
    fn tag_is_crate_prefixed_and_deterministic() {
        let _guard = test_env::set_var("CARGO_PKG_NAME", "demo-crate");
        let tag = invocation_tag(&base());
        let hash = tag.strip_prefix("demo-crate-").expect("crate prefix");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(tag, invocation_tag(&base()));
    }

    #[rstest]
    #[case::dir(ProcessedVesperaInput { folder_name: "admin_routes".to_string(), ..base() })]
    #[case::title(ProcessedVesperaInput { title: Some("Admin".to_string()), ..base() })]
    #[case::version(ProcessedVesperaInput { version: Some("2.0.0".to_string()), ..base() })]
    #[case::openapi(ProcessedVesperaInput { openapi_file_names: vec!["admin.json".to_string()], ..base() })]
    #[case::spec_url(ProcessedVesperaInput { spec_url: Some("/openapi.json".to_string()), ..base() })]
    #[case::docs_url(ProcessedVesperaInput { docs_url: Some("/docs".to_string()), ..base() })]
    #[case::servers(ProcessedVesperaInput {
        servers: Some(vec![vespera_core::openapi::Server {
            url: "https://api.example.com".to_string(),
            description: None,
            variables: None,
        }]),
        ..base()
    })]
    #[case::merge(ProcessedVesperaInput { merge: vec![syn::parse_quote!(child::ChildApp)], ..base() })]
    fn differing_settings_get_distinct_tags(#[case] other: ProcessedVesperaInput) {
        assert_ne!(invocation_tag(&base()), invocation_tag(&other));
    }

    /// The tag must not move with inputs the cache key tracks separately —
    /// otherwise every description edit would orphan the old files.
    #[test]
    fn tag_ignores_package_description() {
        let before = {
            let _guard = test_env::set_var("CARGO_PKG_DESCRIPTION", "first");
            invocation_tag(&base())
        };
        let _guard = test_env::set_var("CARGO_PKG_DESCRIPTION", "second");
        assert_eq!(before, invocation_tag(&base()));
    }
}
