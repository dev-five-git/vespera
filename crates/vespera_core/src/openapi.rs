//! `OpenAPI` document structure definitions

use crate::route::PathItem;
use crate::schema::{Components, ExternalDocumentation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `OpenAPI` document version
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OpenApiVersion {
    #[serde(rename = "3.0.0")]
    V3_0_0,
    #[serde(rename = "3.0.1")]
    V3_0_1,
    #[serde(rename = "3.0.2")]
    V3_0_2,
    #[serde(rename = "3.0.3")]
    V3_0_3,
    #[serde(rename = "3.1.0")]
    #[default]
    V3_1_0,
}

/// Contact information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// Contact name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Contact URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Contact email
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// License information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    /// License name
    pub name: String,
    /// License URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// API information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// API title
    pub title: String,
    /// API version
    pub version: String,
    /// API description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Terms of service URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terms_of_service: Option<String>,
    /// Contact information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<Contact>,
    /// License information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
    /// Summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Server variable
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVariable {
    /// Default value
    pub default: String,
    /// Enum values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<String>>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Server information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    /// Server URL
    pub url: String,
    /// Server description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Server variables.
    ///
    /// `BTreeMap` (not `HashMap`) so the generated OpenAPI output is
    /// deterministic across runs/processes, consistent with the rest of
    /// the document's ordered maps (CORE-01).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, ServerVariable>>,
}

/// Tag definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    /// Tag name
    pub name: String,
    /// Tag description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// External documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,
}

/// `OpenAPI` document (root structure)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApi {
    /// `OpenAPI` version
    pub openapi: OpenApiVersion,
    /// API information
    pub info: Info,
    /// Server list
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<Server>>,
    /// Path definitions
    pub paths: BTreeMap<String, PathItem>,
    /// Components (reusable components)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Components>,
    /// Security requirements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    /// Tag definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<Tag>>,
    /// External documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,
}

/// Merge `other` map entries into `self_map` with self-wins on key
/// conflicts, allocating the target map only when `other` has entries.
fn merge_component_map<V>(
    self_map: &mut Option<BTreeMap<String, V>>,
    other_map: Option<BTreeMap<String, V>>,
) {
    let Some(other_map) = other_map else { return };
    let target = self_map.get_or_insert_with(BTreeMap::new);
    for (name, value) in other_map {
        target.entry(name).or_insert(value);
    }
}

fn has_any_component_map(components: &Components) -> bool {
    components.schemas.is_some()
        || components.responses.is_some()
        || components.parameters.is_some()
        || components.examples.is_some()
        || components.request_bodies.is_some()
        || components.headers.is_some()
        || components.security_schemes.is_some()
}

/// Merge `other`'s per-method operations into `into` with **self-wins**
/// semantics: an operation (or path-level field) already present on `into`
/// is kept; a slot empty on `into` is filled from `other`.
///
/// Applied on a path-key conflict so two apps that define the same path
/// under different methods both keep their operations, instead of the
/// incoming [`PathItem`] being dropped whole.  Destructuring `other` keeps
/// this exhaustive — adding a `PathItem` field forces this to be updated.
fn merge_path_item(into: &mut PathItem, other: PathItem) {
    let PathItem {
        get,
        post,
        put,
        patch,
        delete,
        head,
        options,
        trace,
        parameters,
        summary,
        description,
    } = other;
    if into.get.is_none() {
        into.get = get;
    }
    if into.post.is_none() {
        into.post = post;
    }
    if into.put.is_none() {
        into.put = put;
    }
    if into.patch.is_none() {
        into.patch = patch;
    }
    if into.delete.is_none() {
        into.delete = delete;
    }
    if into.head.is_none() {
        into.head = head;
    }
    if into.options.is_none() {
        into.options = options;
    }
    if into.trace.is_none() {
        into.trace = trace;
    }
    if into.parameters.is_none() {
        into.parameters = parameters;
    }
    if into.summary.is_none() {
        into.summary = summary;
    }
    if into.description.is_none() {
        into.description = description;
    }
}

impl OpenApi {
    /// Merge another `OpenAPI` document into this one.
    ///
    /// All `paths`, `components` (schemas, responses, parameters,
    /// examples, request bodies, headers, security schemes), and `tags`
    /// from `other` are added to `self`. Top-level `servers`, `security`,
    /// and `external_docs` are adopted from `other` only when `self` has
    /// not set its own. On any key/field conflict, `self` takes precedence.
    pub fn merge(&mut self, other: Self) {
        // Merge paths.  On a path-key conflict, merge per HTTP method
        // (self-wins per operation) instead of dropping the incoming
        // `PathItem` wholesale: two merged apps that both define the same
        // path under DIFFERENT methods (parent `GET /users`, child
        // `POST /users`) must keep BOTH operations in the generated
        // document — otherwise the spec under-documents what the merged
        // router actually serves at runtime.
        for (path, item) in other.paths {
            use std::collections::btree_map::Entry;
            match self.paths.entry(path) {
                Entry::Vacant(slot) => {
                    slot.insert(item);
                }
                Entry::Occupied(mut slot) => merge_path_item(slot.get_mut(), item),
            }
        }

        // Merge components (every reusable component kind, self-wins on
        // key conflict) — previously only `schemas` + `security_schemes`
        // were merged, silently dropping the rest.
        if let Some(other_components) = other.components
            && has_any_component_map(&other_components)
        {
            let self_components = self.components.get_or_insert_with(Components::default);

            merge_component_map(&mut self_components.schemas, other_components.schemas);
            merge_component_map(&mut self_components.responses, other_components.responses);
            merge_component_map(&mut self_components.parameters, other_components.parameters);
            merge_component_map(&mut self_components.examples, other_components.examples);
            merge_component_map(
                &mut self_components.request_bodies,
                other_components.request_bodies,
            );
            merge_component_map(&mut self_components.headers, other_components.headers);
            merge_component_map(
                &mut self_components.security_schemes,
                other_components.security_schemes,
            );
        }

        // Merge top-level servers / security / external_docs (self wins:
        // adopt other's only when self has not set its own).
        if self.servers.is_none() {
            self.servers = other.servers;
        }
        if self.security.is_none() {
            self.security = other.security;
        }
        if self.external_docs.is_none() {
            self.external_docs = other.external_docs;
        }

        // Merge tags, de-duplicating by name in a single pass.  `seen` starts
        // with the existing tag names and grows as incoming tags are appended,
        // so an incoming tag is kept only when its name collides with neither
        // an existing tag nor an already-appended incoming one (first-wins,
        // incoming insertion order preserved).  A name is cloned only when the
        // tag is actually kept — a duplicate is detected by borrow and skipped
        // without cloning.
        if let Some(other_tags) = other.tags {
            let self_tags = self.tags.get_or_insert_with(Vec::new);
            let mut seen: std::collections::HashSet<String> =
                self_tags.iter().map(|tag| tag.name.clone()).collect();
            for tag in other_tags {
                if !seen.contains(tag.name.as_str()) {
                    seen.insert(tag.name.clone());
                    self_tags.push(tag);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::{Operation, PathItem};
    use crate::schema::{Components, Schema, SchemaType, SecurityScheme, SecuritySchemeType};

    fn create_base_openapi() -> OpenApi {
        OpenApi {
            openapi: OpenApiVersion::V3_1_0,
            info: Info {
                title: "Base API".to_string(),
                version: "1.0.0".to_string(),
                description: None,
                terms_of_service: None,
                contact: None,
                license: None,
                summary: None,
            },
            servers: None,
            paths: BTreeMap::new(),
            components: None,
            security: None,
            tags: None,
            external_docs: None,
        }
    }

    fn create_path_item(summary: &str) -> PathItem {
        PathItem {
            get: Some(Operation {
                summary: Some(summary.to_string()),
                description: None,
                operation_id: None,
                tags: None,
                parameters: None,
                request_body: None,
                responses: BTreeMap::new(),
                security: None,
                deprecated: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_merge_paths() {
        let mut base = create_base_openapi();
        base.paths
            .insert("/users".to_string(), create_path_item("Get users"));

        let mut other = create_base_openapi();
        other
            .paths
            .insert("/posts".to_string(), create_path_item("Get posts"));
        other
            .paths
            .insert("/users".to_string(), create_path_item("Other users")); // Conflict

        base.merge(other);

        // Both paths should exist
        assert!(base.paths.contains_key("/users"));
        assert!(base.paths.contains_key("/posts"));
        // Self takes precedence on conflict
        assert_eq!(
            base.paths
                .get("/users")
                .unwrap()
                .get
                .as_ref()
                .unwrap()
                .summary,
            Some("Get users".to_string())
        );
    }

    fn create_post_path_item(summary: &str) -> PathItem {
        PathItem {
            post: Some(Operation {
                summary: Some(summary.to_string()),
                description: None,
                operation_id: None,
                tags: None,
                parameters: None,
                request_body: None,
                responses: BTreeMap::new(),
                security: None,
                deprecated: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_merge_same_path_different_methods_are_combined() {
        // Regression: a path-key conflict must merge per HTTP method, not
        // drop the incoming PathItem wholesale.  Parent defines GET /users,
        // child defines POST /users — the merged document must expose BOTH
        // operations (otherwise the spec under-documents the merged router).
        let mut base = create_base_openapi();
        base.paths
            .insert("/users".to_string(), create_path_item("List users")); // GET

        let mut other = create_base_openapi();
        other
            .paths
            .insert("/users".to_string(), create_post_path_item("Create user")); // POST

        base.merge(other);

        let users = base.paths.get("/users").expect("/users present");
        // self-wins GET is preserved
        assert_eq!(
            users.get.as_ref().unwrap().summary,
            Some("List users".to_string())
        );
        // incoming POST is merged in (previously dropped on the whole-item
        // `or_insert`)
        assert_eq!(
            users.post.as_ref().unwrap().summary,
            Some("Create user".to_string())
        );
    }

    #[test]
    fn test_merge_same_path_same_method_self_wins() {
        // Same path AND same method on both sides: self's operation is kept,
        // the incoming one is discarded.
        let mut base = create_base_openapi();
        base.paths
            .insert("/users".to_string(), create_path_item("Base get"));

        let mut other = create_base_openapi();
        other
            .paths
            .insert("/users".to_string(), create_path_item("Other get"));

        base.merge(other);

        assert_eq!(
            base.paths
                .get("/users")
                .unwrap()
                .get
                .as_ref()
                .unwrap()
                .summary,
            Some("Base get".to_string())
        );
    }

    #[test]
    fn test_merge_schemas() {
        let mut base = create_base_openapi();
        let mut base_schemas = BTreeMap::new();
        base_schemas.insert("User".to_string(), Schema::object());
        base.components = Some(Components {
            schemas: Some(base_schemas),
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        let mut other = create_base_openapi();
        let mut other_schemas = BTreeMap::new();
        other_schemas.insert("Post".to_string(), Schema::object());
        other_schemas.insert("User".to_string(), Schema::string()); // Conflict
        other.components = Some(Components {
            schemas: Some(other_schemas),
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        base.merge(other);

        let schemas = base.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
        assert!(schemas.contains_key("Post"));
        // Self takes precedence on conflict
        assert_eq!(
            schemas.get("User").unwrap().schema_type,
            Some(SchemaType::Object)
        );
    }

    #[test]
    fn test_merge_schemas_when_self_has_no_components() {
        let mut base = create_base_openapi();
        assert!(base.components.is_none());

        let mut other = create_base_openapi();
        let mut other_schemas = BTreeMap::new();
        other_schemas.insert("Post".to_string(), Schema::object());
        other.components = Some(Components {
            schemas: Some(other_schemas),
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        base.merge(other);

        assert!(base.components.is_some());
        let schemas = base.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Post"));
    }

    #[test]
    fn test_merge_security_schemes() {
        let mut base = create_base_openapi();
        let mut base_security_schemes = BTreeMap::new();
        base_security_schemes.insert(
            "bearerAuth".to_string(),
            SecurityScheme {
                r#type: SecuritySchemeType::Http,
                description: None,
                name: None,
                r#in: None,
                scheme: Some("bearer".to_string()),
                bearer_format: Some("JWT".to_string()),
            },
        );
        base.components = Some(Components {
            schemas: None,
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: Some(base_security_schemes),
        });

        let mut other = create_base_openapi();
        let mut other_security_schemes = BTreeMap::new();
        other_security_schemes.insert(
            "apiKey".to_string(),
            SecurityScheme {
                r#type: SecuritySchemeType::ApiKey,
                description: None,
                name: Some("X-API-Key".to_string()),
                r#in: Some("header".to_string()),
                scheme: None,
                bearer_format: None,
            },
        );
        other.components = Some(Components {
            schemas: None,
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: Some(other_security_schemes),
        });

        base.merge(other);

        let security_schemes = base
            .components
            .as_ref()
            .unwrap()
            .security_schemes
            .as_ref()
            .unwrap();
        assert!(security_schemes.contains_key("bearerAuth"));
        assert!(security_schemes.contains_key("apiKey"));
    }

    #[test]
    fn test_merge_tags() {
        let mut base = create_base_openapi();
        base.tags = Some(vec![Tag {
            name: "users".to_string(),
            description: Some("User operations".to_string()),
            external_docs: None,
        }]);

        let mut other = create_base_openapi();
        other.tags = Some(vec![
            Tag {
                name: "posts".to_string(),
                description: Some("Post operations".to_string()),
                external_docs: None,
            },
            Tag {
                name: "users".to_string(),
                description: Some("Duplicate users tag".to_string()),
                external_docs: None,
            }, // Duplicate
        ]);

        base.merge(other);

        let tags = base.tags.as_ref().unwrap();
        assert_eq!(tags.len(), 2); // No duplicates
        assert!(tags.iter().any(|t| t.name == "users"));
        assert!(tags.iter().any(|t| t.name == "posts"));
        // Self's description takes precedence
        let users_tag = tags.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(users_tag.description, Some("User operations".to_string()));
    }

    #[test]
    fn test_merge_tags_when_self_has_none() {
        let mut base = create_base_openapi();
        assert!(base.tags.is_none());

        let mut other = create_base_openapi();
        other.tags = Some(vec![Tag {
            name: "posts".to_string(),
            description: None,
            external_docs: None,
        }]);

        base.merge(other);

        assert!(base.tags.is_some());
        assert_eq!(base.tags.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_merge_empty_other() {
        let mut base = create_base_openapi();
        base.paths
            .insert("/users".to_string(), create_path_item("Get users"));
        base.tags = Some(vec![Tag {
            name: "users".to_string(),
            description: None,
            external_docs: None,
        }]);

        let other = create_base_openapi(); // Empty paths, no components, no tags

        base.merge(other);

        // Base should remain unchanged
        assert_eq!(base.paths.len(), 1);
        assert!(base.paths.contains_key("/users"));
        assert_eq!(base.tags.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_merge_components_responses_and_parameters() {
        use crate::route::{Parameter, ParameterLocation, Response};

        let response = |desc: &str| Response {
            description: desc.to_string(),
            headers: None,
            content: None,
        };

        let mut base = create_base_openapi();
        base.components = Some(Components {
            schemas: None,
            responses: Some(BTreeMap::from([("NotFound".to_string(), response("base"))])),
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        let mut other = create_base_openapi();
        other.components = Some(Components {
            schemas: None,
            responses: Some(BTreeMap::from([
                ("NotFound".to_string(), response("other-dup")),
                ("ServerError".to_string(), response("other")),
            ])),
            parameters: Some(BTreeMap::from([(
                "PageParam".to_string(),
                Parameter {
                    name: "page".to_string(),
                    r#in: ParameterLocation::Query,
                    description: None,
                    required: None,
                    schema: None,
                    example: None,
                },
            )])),
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        base.merge(other);

        let comps = base.components.as_ref().unwrap();
        let responses = comps.responses.as_ref().unwrap();
        // other's non-conflicting response is merged in (previously dropped).
        assert!(responses.contains_key("NotFound"));
        assert!(responses.contains_key("ServerError"));
        // self wins on conflict.
        assert_eq!(responses.get("NotFound").unwrap().description, "base");
        // parameters adopted from other (base had none) — previously dropped.
        assert!(comps.parameters.as_ref().unwrap().contains_key("PageParam"));
    }

    #[test]
    fn test_merge_top_level_servers_security_external_docs() {
        use crate::schema::ExternalDocumentation;

        // base sets none of the three → adopts other's.
        let mut base = create_base_openapi();
        let mut other = create_base_openapi();
        other.servers = Some(vec![Server {
            url: "https://api.example.com".to_string(),
            description: None,
            variables: None,
        }]);
        other.security = Some(vec![BTreeMap::from([(
            "bearerAuth".to_string(),
            Vec::new(),
        )])]);
        other.external_docs = Some(ExternalDocumentation {
            description: None,
            url: "https://docs.example.com".to_string(),
        });

        base.merge(other);

        assert_eq!(
            base.servers.as_ref().unwrap()[0].url,
            "https://api.example.com"
        );
        assert!(base.security.is_some());
        assert_eq!(
            base.external_docs.as_ref().unwrap().url,
            "https://docs.example.com"
        );

        // self-wins: base already has servers → other's ignored.
        let mut base2 = create_base_openapi();
        base2.servers = Some(vec![Server {
            url: "https://self.example.com".to_string(),
            description: None,
            variables: None,
        }]);
        let mut other2 = create_base_openapi();
        other2.servers = Some(vec![Server {
            url: "https://other.example.com".to_string(),
            description: None,
            variables: None,
        }]);
        base2.merge(other2);
        assert_eq!(
            base2.servers.as_ref().unwrap()[0].url,
            "https://self.example.com"
        );
    }
}
