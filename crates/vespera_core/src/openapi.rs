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
    let Some(other_map) = non_empty_component_map(other_map) else {
        return;
    };
    let target = self_map.get_or_insert_with(BTreeMap::new);
    for (name, value) in other_map {
        target.entry(name).or_insert(value);
    }
}

fn non_empty_component_map<V>(map: Option<BTreeMap<String, V>>) -> Option<BTreeMap<String, V>> {
    map.filter(|entries| !entries.is_empty())
}

fn has_any_component_map(components: &Components) -> bool {
    components
        .schemas
        .as_ref()
        .is_some_and(|entries| !entries.is_empty())
        || components
            .responses
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .parameters
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .examples
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .request_bodies
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .headers
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .security_schemes
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
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

        // Merge tags, de-duplicating by name with first-wins semantics while
        // preserving deterministic output order (existing tags first, then
        // incoming tags in their original order).
        //
        // A linear `any` scan beats a `HashSet<String>` here: tag sets are
        // tiny (OpenAPI tags are top-level operation groupings — a handful,
        // rarely past a few dozen even for large APIs), so the O(n²) short-
        // string compare over an already-resident `Vec` is cheaper than
        // allocating a set and cloning every existing + incoming tag name.
        // Net: zero allocations and zero `String` clones on the merge path.
        if let Some(other_tags) = other.tags {
            let self_tags = self.tags.get_or_insert_with(Vec::new);
            for tag in other_tags {
                if !self_tags.iter().any(|existing| existing.name == tag.name) {
                    self_tags.push(tag);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
