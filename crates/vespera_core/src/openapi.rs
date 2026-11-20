//! `OpenAPI` document structure definitions

use crate::route::{Operation, PathItem};
use crate::schema::{Components, ExternalDocumentation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `OpenAPI` document version
///
/// Deserializing maps every `3.0.x` to [`OpenApiVersion::V3_1_0`]: the
/// document body is read into OpenAPI 3.1 shapes (`nullable` →
/// `type: [T, "null"]`, `example` → `examples`, boolean
/// `exclusiveMinimum` → a number), so a kept `3.0.x` would label 3.1-only
/// output as 3.0.  The `3.0.x` variants still serialize as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(from = "OpenApiVersionWire")]
pub enum OpenApiVersion {
    #[serde(rename = "3.0.0")]
    V3_0_0,
    #[serde(rename = "3.0.1")]
    V3_0_1,
    #[serde(rename = "3.0.2")]
    V3_0_2,
    #[serde(rename = "3.0.3")]
    V3_0_3,
    #[serde(rename = "3.0.4")]
    V3_0_4,
    #[serde(rename = "3.1.0")]
    #[default]
    V3_1_0,
    #[serde(rename = "3.1.1")]
    V3_1_1,
    #[serde(rename = "3.1.2")]
    V3_1_2,
}

/// The accepted `openapi` strings (deserialize only; see [`OpenApiVersion`]).
#[derive(Deserialize)]
enum OpenApiVersionWire {
    #[serde(
        rename = "3.0.0",
        alias = "3.0.1",
        alias = "3.0.2",
        alias = "3.0.3",
        alias = "3.0.4"
    )]
    V3_0,
    #[serde(rename = "3.1.0")]
    V3_1_0,
    #[serde(rename = "3.1.1")]
    V3_1_1,
    #[serde(rename = "3.1.2")]
    V3_1_2,
}

impl From<OpenApiVersionWire> for OpenApiVersion {
    fn from(wire: OpenApiVersionWire) -> Self {
        match wire {
            OpenApiVersionWire::V3_0 | OpenApiVersionWire::V3_1_0 => Self::V3_1_0,
            OpenApiVersionWire::V3_1_1 => Self::V3_1_1,
            OpenApiVersionWire::V3_1_2 => Self::V3_1_2,
        }
    }
}

/// Contact information
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
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
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    /// License name
    pub name: String,
    /// SPDX license expression or identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// License URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[allow(clippy::ref_option)] // serde skip_serializing_if mandates &Option<T> signature
fn is_empty_components(value: &Option<Components>) -> bool {
    value
        .as_ref()
        .is_none_or(|components| !has_any_component_map(components))
}

/// API information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// API title
    pub title: String,
    /// API version
    pub version: String,
    /// Short summary of the API
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
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
    /// Specification extensions (`x-*` keys only, e.g. `x-logo`).
    #[serde(flatten, with = "crate::extensions")]
    pub extensions: crate::Extensions,
}

/// Server variable
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
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
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
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
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// (optional in 3.1: a webhooks- or components-only document omits it;
    /// always serialized, as `{}` when empty; the Paths Object's `x-*`
    /// extension keys are skipped on deserialize, never read as paths)
    #[serde(default, deserialize_with = "crate::extensions::skip_extension_keys")]
    pub paths: BTreeMap<String, PathItem>,
    /// Webhooks (OpenAPI 3.1): incoming requests the API *sends* to
    /// consumers, keyed by a unique webhook name.
    ///
    /// `vespera!` never fills this; it exists so a post-processed document
    /// can declare webhooks and so specs that carry them round-trip. Like the
    /// Paths Object, its `x-*` extension keys are skipped on deserialize.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::extensions::skip_optional_extension_keys"
    )]
    pub webhooks: Option<BTreeMap<String, PathItem>>,
    /// Components (reusable components)
    #[serde(skip_serializing_if = "is_empty_components")]
    pub components: Option<Components>,
    /// Security requirements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<crate::schema::SecurityRequirement>>,
    /// Tag definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<Tag>>,
    /// External documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,
    /// Specification extensions (`x-*` keys only, e.g. `x-tagGroups`).
    #[serde(flatten, with = "crate::extensions")]
    pub extensions: crate::Extensions,
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
        || components
            .links
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .callbacks
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        || components
            .path_items
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
}

/// The eight per-method operation slots of `item`, in declaration order.
fn operation_slots_mut(item: &mut PathItem) -> [&mut Option<Operation>; 8] {
    [
        &mut item.get,
        &mut item.post,
        &mut item.put,
        &mut item.patch,
        &mut item.delete,
        &mut item.head,
        &mut item.options,
        &mut item.trace,
    ]
}

/// Move `item`'s path-level `parameters` / `servers` onto its own
/// operations, leaving both path-level fields `None`.
///
/// An operation-level parameter with the same `name` + `in` overrides the
/// path-level one (OpenAPI 3.1, Path Item Object), so it is kept; an
/// operation with its own `servers` keeps them (they override the path-level
/// list).  The resulting item describes exactly the same requests.
fn push_path_level_down(item: &mut PathItem) {
    let parameters = item.parameters.take();
    let servers = item.servers.take();
    for op in operation_slots_mut(item).into_iter().flatten() {
        if let Some(path_params) = parameters.as_ref().filter(|params| !params.is_empty()) {
            let op_params = op.parameters.get_or_insert_with(Vec::new);
            for param in path_params {
                if !op_params
                    .iter()
                    .any(|own| own.name == param.name && own.r#in == param.r#in)
                {
                    op_params.push(param.clone());
                }
            }
        }
        if op.servers.is_none() {
            op.servers.clone_from(&servers);
        }
    }
}

/// Merge `other`'s per-method operations into `into` with **self-wins**
/// semantics: an operation (or path-level field) already present on `into`
/// is kept; a slot empty on `into` is filled from `other`.
///
/// Applied on a path-key conflict so two apps that define the same path
/// under different methods both keep their operations, instead of the
/// incoming [`PathItem`] being dropped whole.
///
/// Path-level `parameters` / `servers` apply to every operation of their
/// item, so they must not cross over to the other side's operations: when
/// `other` contributes operations and `into` has operations or path-level
/// values of its own, each side's path-level values are first pushed down
/// onto its own operations (an operation-less `into`'s values govern
/// nothing, so they are dropped rather than adopted by `other`'s
/// operations); when only `into` keeps operations, `other`'s path-level
/// values are dropped (they governed nothing that survives).  Destructuring
/// `other` keeps this exhaustive — adding a `PathItem` field forces this to
/// be updated.
///
/// A `$ref` never ends up next to operations (OpenAPI 3.1 leaves a
/// reference with conflicting siblings undefined): `merge` inlines resolvable
/// `#/components/pathItems/*` references of both sides first, an `into` that
/// is still a reference keeps itself over `other`'s operations (self-wins —
/// its operations live behind the reference), and `other`'s reference is
/// adopted only by an `into` that has no operations — together with
/// `other`'s path-level `parameters` / `servers`, replacing `into`'s.
fn merge_path_item(into: &mut PathItem, mut other: PathItem) {
    if into.ref_path.is_some() {
        if has_operations(&mut other) {
            return;
        }
        // `into`'s operations live behind its (unresolvable) reference:
        // `other`'s path-level values governed none of them.
        other.parameters = None;
        other.servers = None;
    }
    let into_has_ops = has_operations(into);
    let into_has_path_level = into.parameters.is_some() || into.servers.is_some();
    let other_contributes = operation_slots_mut(into)
        .into_iter()
        .zip(operation_slots_mut(&mut other))
        .any(|(own, incoming)| own.is_none() && incoming.is_some());
    if other_contributes && (into_has_ops || into_has_path_level) {
        push_path_level_down(into);
        push_path_level_down(&mut other);
    } else if into_has_ops {
        other.parameters = None;
        other.servers = None;
    } else if into.ref_path.is_none() && other.ref_path.is_some() && !has_operations(&mut other) {
        // `into` (no operations) adopts `other`'s reference below: its own
        // path-level values governed nothing, while `other`'s sit next to the
        // reference and govern the operations behind it.
        into.parameters = None;
        into.servers = None;
    }
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
        ref_path,
        servers,
        extensions,
    } = other;
    for (key, value) in extensions {
        into.extensions.entry(key).or_insert(value);
    }
    for (slot, incoming) in operation_slots_mut(into)
        .into_iter()
        .zip([get, post, put, patch, delete, head, options, trace])
    {
        if slot.is_none() {
            *slot = incoming;
        }
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
    if into.ref_path.is_none() && !has_operations(into) {
        into.ref_path = ref_path;
    }
    if into.servers.is_none() {
        into.servers = servers;
    }
}

/// Whether a `components.pathItems` name of `other` is also one of `self`'s.
/// Then an incoming `#/components/pathItems/*` reference that `merge` adopts
/// unresolved would, after the merge, resolve to `self`'s component of that
/// name (self wins the component merge), not to `other`'s.
fn shares_path_item_name(
    theirs: Option<&BTreeMap<String, PathItem>>,
    mine: Option<&BTreeMap<String, PathItem>>,
) -> bool {
    match (theirs, mine) {
        (Some(theirs), Some(mine)) => theirs.keys().any(|name| mine.contains_key(name)),
        _ => false,
    }
}

fn has_operations(item: &mut PathItem) -> bool {
    operation_slots_mut(item).iter().any(|op| op.is_some())
}

/// Replace a path item that only points at `#/components/pathItems/<name>`
/// with an inline copy of that component, so the top-level push-down below
/// reaches its operations.  Writing into the shared component instead would
/// also change every other reference to it — including the receiver's own,
/// which `merge` keeps on a component-name conflict.  A chain of references
/// is followed; a cycle (a component seen twice on the chain) leaves the item
/// exactly as it was, the reference included, whatever the number of
/// components.  A reference this document cannot resolve (missing name,
/// external URI) is left as it is.
///
/// Path-level `parameters` / `servers` written next to the reference keep
/// governing the inlined operations: they are combined with the target's
/// own path-level values, the referencing item winning (a parameter on
/// `name` + `in`, the server list as a whole).
fn inline_path_item_ref(item: &mut PathItem, path_items: &BTreeMap<String, PathItem>) {
    let original = item.clone();
    let mut visited: Vec<&str> = Vec::new();
    loop {
        let Some((name, target)) = item
            .ref_path
            .as_deref()
            .and_then(|r| r.strip_prefix("#/components/pathItems/"))
            .and_then(|name| path_items.get_key_value(name))
        else {
            // Nothing (more) to inline.  A chain that dangles after a step
            // (the item still holds a reference) is restored: the reference
            // the item started from is the one the document wrote, not the
            // last name of the broken chain.  A fully inlined item is kept.
            if !visited.is_empty() && item.ref_path.is_some() {
                *item = original;
            }
            return;
        };
        if visited.contains(&name.as_str()) {
            *item = original;
            return;
        }
        visited.push(name.as_str());
        item.ref_path = None;
        let own_parameters = item.parameters.take();
        let own_servers = item.servers.take();
        merge_path_item(item, target.clone());
        if let Some(own) = own_parameters {
            let inherited = item.parameters.take().unwrap_or_default();
            let mut combined = own;
            for param in inherited {
                if !combined
                    .iter()
                    .any(|mine| mine.name == param.name && mine.r#in == param.r#in)
                {
                    combined.push(param);
                }
            }
            item.parameters = Some(combined);
        }
        if own_servers.is_some() {
            item.servers = own_servers;
        }
    }
}

/// Push `other`'s top-level `security` / `servers` down onto each of its own
/// operations that has not set one, so they keep governing exactly the
/// operations they governed in `other` once merged into a document whose
/// top level they must not touch.
///
/// When `other` has no top-level `security` but the receiver requires some
/// (`receiver_requires_security`), an operation without its own `security`
/// gets an explicit empty list: in `other` it was unauthenticated, and the
/// receiver's requirement must not start applying to it.
///
/// Servers are not pushed onto the operations of a path item with its own
/// `servers` (path-level servers already override the top-level list there);
/// webhooks pass `servers: None` — their operations are requests the API
/// *sends*, which the API's server list does not describe.  A path item that
/// is a `$ref` to `other`'s `components.pathItems` is inlined first (see
/// [`inline_path_item_ref`]) when there is anything to push down.
fn push_down_top_level(
    paths: &mut BTreeMap<String, PathItem>,
    path_items: Option<&BTreeMap<String, PathItem>>,
    security: Option<&Vec<crate::schema::SecurityRequirement>>,
    servers: Option<&Vec<Server>>,
    receiver_requires_security: bool,
) {
    let security = match security {
        Some(own) => Some(own.clone()),
        None if receiver_requires_security => Some(Vec::new()),
        None => None,
    };
    let path_items = path_items.filter(|_| security.is_some() || servers.is_some());
    for item in paths.values_mut() {
        if let Some(path_items) = path_items {
            inline_path_item_ref(item, path_items);
        }
        let servers = if item.servers.is_some() {
            None
        } else {
            servers
        };
        for op in operation_slots_mut(item).into_iter().flatten() {
            if op.security.is_none() {
                op.security.clone_from(&security);
            }
            if op.servers.is_none() {
                op.servers = servers.cloned();
            }
        }
    }
}

impl OpenApi {
    /// Merge another `OpenAPI` document into this one.
    ///
    /// All `paths`, `components` (schemas, responses, parameters,
    /// examples, request bodies, headers, security schemes, links,
    /// callbacks, path items), webhooks, `tags` and `x-*` extensions from
    /// `other` are added to `self`. `other`'s top-level `security` and
    /// `servers` are never adopted (they would then govern every operation
    /// of `self`): they are pushed down onto each of `other`'s operations
    /// that has not set its own (only `security` onto webhooks). When
    /// `other` has no top-level `security` but `self` requires some, those
    /// operations get an explicit empty `security` instead, so `self`'s
    /// requirement does not start applying to them. A path item or webhook
    /// of `other` that is a `$ref` to `other`'s `components.pathItems` is
    /// inlined for this push-down (a reference `other` cannot resolve is
    /// kept, and its operations are not reached). A kept reference whose
    /// target name `other` does not define is adopted by name: when `self`
    /// defines a `components.pathItems` entry of that name, the adopted
    /// reference then resolves to `self`'s entry, which the document cannot
    /// tell apart from one `other` meant. Inlining is impossible (the target
    /// is unknown), so this is the chosen behaviour. When `other` and `self`
    /// share a `components.pathItems` name, every such reference is inlined
    /// (also when it is adopted as a new path or webhook), so it cannot
    /// resolve to `self`'s component of that name. On a shared path, each
    /// side's path-level `parameters` / `servers` stay with its own
    /// operations (see `merge_path_item`). Top-level `external_docs` is adopted only
    /// when `self` has none. On any key/field conflict, `self` takes
    /// precedence.
    pub fn merge(&mut self, mut other: Self) {
        let receiver_requires_security = self
            .security
            .as_ref()
            .is_some_and(|requirements| !requirements.is_empty());
        let path_items = other
            .components
            .as_ref()
            .and_then(|c| c.path_items.as_ref());
        push_down_top_level(
            &mut other.paths,
            path_items,
            other.security.as_ref(),
            other.servers.as_ref(),
            receiver_requires_security,
        );
        if let Some(webhooks) = other.webhooks.as_mut() {
            push_down_top_level(
                webhooks,
                path_items,
                other.security.as_ref(),
                None,
                receiver_requires_security,
            );
        }

        // Merge paths.  On a path-key conflict, merge per HTTP method
        // (self-wins per operation) instead of dropping the incoming
        // `PathItem` wholesale: two merged apps that both define the same
        // path under DIFFERENT methods (parent `GET /users`, child
        // `POST /users`) must keep BOTH operations in the generated
        // document — otherwise the spec under-documents what the merged
        // router actually serves at runtime.
        //
        // On a conflict, both sides' `#/components/pathItems/*` references
        // are inlined first (whatever the push-down above decided), so a
        // `$ref` never lands next to the other side's operations.
        let self_path_items = self.components.as_ref().and_then(|c| c.path_items.as_ref());
        // With a shared name, an adopted reference is inlined too (see
        // `shares_path_item_name`); an unrelated one keeps its `$ref`.
        let name_clash = shares_path_item_name(path_items, self_path_items);
        for (path, mut item) in other.paths {
            use std::collections::btree_map::Entry;
            match self.paths.entry(path) {
                Entry::Vacant(slot) => {
                    if name_clash && let Some(path_items) = path_items {
                        inline_path_item_ref(&mut item, path_items);
                    }
                    slot.insert(item);
                }
                Entry::Occupied(mut slot) => {
                    let into = slot.get_mut();
                    if let Some(path_items) = path_items {
                        inline_path_item_ref(&mut item, path_items);
                    }
                    if let Some(path_items) = self_path_items {
                        inline_path_item_ref(into, path_items);
                    }
                    merge_path_item(into, item);
                }
            }
        }

        // Webhooks: union by name, self-wins on a name conflict.  Before the
        // components move below: an adopted reference reads `path_items`.
        if let Some(other_webhooks) = other.webhooks {
            let self_webhooks = self.webhooks.get_or_insert_with(BTreeMap::new);
            for (name, mut item) in other_webhooks {
                if name_clash && let Some(path_items) = path_items {
                    inline_path_item_ref(&mut item, path_items);
                }
                self_webhooks.entry(name).or_insert(item);
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
            merge_component_map(&mut self_components.links, other_components.links);
            merge_component_map(&mut self_components.callbacks, other_components.callbacks);
            merge_component_map(&mut self_components.path_items, other_components.path_items);
        }

        // Top-level servers / security were pushed down onto other's
        // operations above; external_docs is self-wins.
        if self.external_docs.is_none() {
            self.external_docs = other.external_docs;
        }
        // Top-level `x-*` extensions: union by key, self-wins.
        for (key, value) in other.extensions {
            self.extensions.entry(key).or_insert(value);
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
#[cfg(test)]
mod tests_merge_scope;
#[cfg(test)]
mod tests_spec_fields;
