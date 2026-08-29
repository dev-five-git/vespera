use proc_macro2::Span;
use vespera_core::openapi::OpenApi;

use crate::{
    error::MacroResult,
    metadata::{CollectedMetadata, StructMetadata},
};

/// Tracks component-schema definitions and their source while exported apps
/// are folded into a parent document.
pub(super) struct SchemaMergeGuard {
    metadata: CollectedMetadata,
}

impl SchemaMergeGuard {
    pub(super) fn new(parent: &OpenApi) -> MacroResult<Self> {
        let mut guard = Self {
            metadata: CollectedMetadata::new(),
        };
        guard.record(parent, "the parent app", Span::call_site())?;
        Ok(guard)
    }

    /// Reject a child whose component name is already attached to a different
    /// JSON Schema. Equal definitions are intentionally retained as normal
    /// deduplication.
    pub(super) fn check_child(
        &mut self,
        child: &OpenApi,
        child_origin: &str,
        span: Span,
    ) -> MacroResult<()> {
        self.record(child, child_origin, span)
    }

    fn record(&mut self, document: &OpenApi, origin: &str, span: Span) -> MacroResult<()> {
        let Some(schemas) = document
            .components
            .as_ref()
            .and_then(|components| components.schemas.as_ref())
        else {
            return Ok(());
        };

        for (name, schema) in schemas {
            let definition = serde_json::to_string(schema).map_err(|error| {
                syn::Error::new(
                    span,
                    format!(
                        "OpenAPI merge: failed to compare schema `{name}` from {origin}. Error: {error}."
                    ),
                )
            })?;

            self.metadata.structs.push(
                StructMetadata::new(name.clone(), definition)
                    .with_source_identity(origin.to_string()),
            );
        }

        self.metadata
            .check_duplicate_schema_names()
            .map_err(|message| syn::Error::new(span, format!("OpenAPI merge: {message}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(schema: &serde_json::Value) -> OpenApi {
        serde_json::from_value(serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "test", "version": "1.0.0" },
            "paths": {},
            "components": { "schemas": { "ExampleItem": schema } }
        }))
        .unwrap()
    }

    #[test]
    fn different_same_named_schemas_are_rejected_with_both_origins() {
        let parent = document(&serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } }
        }));
        let child = document(&serde_json::json!({
            "type": "object",
            "properties": { "collisionMarker": { "type": "boolean" } }
        }));
        let mut guard = SchemaMergeGuard::new(&parent).unwrap();

        let error = guard
            .check_child(&child, "merged app `plugin_b::PluginB`", Span::call_site())
            .expect_err("different definitions must fail");
        let message = error.to_string();

        assert!(message.contains("Duplicate OpenAPI schema name 'ExampleItem'"));
        assert!(message.contains("plugin_b::PluginB"));
        assert!(message.contains("parent app"));
    }

    #[test]
    fn identical_same_named_schemas_are_accepted() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "error": { "type": "string" },
                "code": { "type": "integer" }
            },
            "required": ["error", "code"]
        });
        let parent = document(&schema);
        let child = document(&schema);
        let mut guard = SchemaMergeGuard::new(&parent).unwrap();

        guard
            .check_child(&child, "merged app `plugin::Plugin`", Span::call_site())
            .expect("identical definitions should deduplicate");
    }

    #[test]
    fn a_schema_defined_once_is_unaffected() {
        let parent = document(&serde_json::json!({ "type": "string" }));

        SchemaMergeGuard::new(&parent).expect("one definition should be accepted");
    }

    #[test]
    fn child_to_child_conflict_reports_the_first_child() {
        let empty: OpenApi = serde_json::from_value(serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "test", "version": "1.0.0" },
            "paths": {}
        }))
        .unwrap();
        let first = document(&serde_json::json!({ "type": "string" }));
        let second = document(&serde_json::json!({ "type": "integer" }));
        let mut guard = SchemaMergeGuard::new(&empty).unwrap();
        guard
            .check_child(&first, "merged app `plugin_a::PluginA`", Span::call_site())
            .unwrap();

        let error = guard
            .check_child(&second, "merged app `plugin_b::PluginB`", Span::call_site())
            .expect_err("the later child must conflict with the first");
        let message = error.to_string();

        assert!(message.contains("plugin_a::PluginA"));
        assert!(message.contains("plugin_b::PluginB"));
    }
}
