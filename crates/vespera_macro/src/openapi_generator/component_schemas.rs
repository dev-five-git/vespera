//! Component schema lookup, file-cache indexing, and schema parsing.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

use crate::{
    metadata::CollectedMetadata,
    openapi_generator::{defaults::process_default_functions_for, paths::parallel_filter_map},
    parser::{
        parse_enum_to_schema, parse_struct_to_schema,
        schema::{SerdeSide, registry_keys, request_view},
    },
};

/// The `{Name}Input` request-view names for the documented types (see
/// [`request_view`]). A view can only differ through a `serde` attribute,
/// a `#[form_data(default)]`, a one-sided `Serialize_repr` /
/// `Deserialize_repr` derive, a `HashSet` / `BTreeSet` / `Duration` /
/// `Decimal` type (`request_view::type_view_differs`) or an externally
/// tagged enum mixing unit and data variants (`{"Unit": null}` is also
/// read), so definitions mentioning none of them (and no `enum`) are not
/// even parsed.
pub(super) fn input_names(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<&str>,
    scoped: &[ScopedRustIdent<'_>],
) -> request_view::InputNames {
    let documented: Vec<&crate::metadata::StructMetadata> = metadata
        .structs
        .iter()
        .filter(|s| s.include_in_openapi)
        .collect();
    if !documented.iter().any(|s| {
        [
            "serde",
            "form_data",
            "_repr",
            "Set",
            "Duration",
            "Decimal",
            "enum",
        ]
        .iter()
        .any(|needle| s.definition.contains(needle))
    }) {
        return std::sync::Arc::default();
    }
    let items: Vec<Option<syn::Item>> = documented
        .iter()
        .map(|s| syn::parse_str::<syn::Item>(&s.definition).ok())
        .collect();
    let sources: Vec<request_view::ViewSource<'_>> = documented
        .iter()
        .zip(&items)
        .filter_map(|(meta, item)| {
            Some(request_view::ViewSource {
                name: &meta.name,
                rust_ident: meta.rust_ident.as_deref(),
                scoped_keys: scoped
                    .iter()
                    .filter(|entry| entry.name == meta.name)
                    .flat_map(|entry| entry.keys.iter().map(String::as_str))
                    .collect(),
                item: item.as_ref()?,
            })
        })
        .collect();
    let taken: BTreeSet<String> = known_schema_names.iter().map(ToString::to_string).collect();
    request_view::compute_input_names(&sources, &taken)
}

/// `input_names` without the inputs whose built schema equals the response
/// view's, or `None` when every input differs.
pub(super) fn without_identical_inputs(
    input_names: &request_view::InputNames,
    schemas: &BTreeMap<String, vespera_core::schema::Schema>,
) -> Option<request_view::InputNames> {
    let as_json = |name: &str| schemas.get(name).map(serde_json::to_value);
    let identical: BTreeSet<&String> = input_names
        .iter()
        .filter(|(component, input)| {
            schemas.contains_key(component.as_str())
                && as_json(component).map(Result::ok) == as_json(input).map(Result::ok)
        })
        .map(|(_, input)| input)
        .collect();
    if identical.is_empty() {
        return None;
    }
    Some(std::sync::Arc::new(
        input_names.without_inputs(|input| identical.contains(input)),
    ))
}

/// Drop the `{Name}Input` components no request body reaches: a type that
/// only ever appears in responses keeps a single component.  The request-view
/// generic instances materialised while building them (`Tree_i32.Input`,
/// never in `input_names`; no other component name holds a `.`) are pruned
/// the same way.
pub(super) fn prune_unreferenced_inputs(
    schemas: &mut BTreeMap<String, vespera_core::schema::Schema>,
    paths: &BTreeMap<String, vespera_core::route::PathItem>,
    input_names: &request_view::InputNames,
) {
    let inputs: BTreeSet<&str> = input_names.values().map(String::as_str).collect();
    let prunable = |name: &str| inputs.contains(name) || name.ends_with(".Input");
    if !schemas.keys().any(|name| prunable(name)) {
        return;
    }
    let reached = reach(schemas, path_refs(paths).request);
    schemas.retain(|name, _| !prunable(name) || reached.contains(name));
}

/// Give a request-only type its own name back: when a request body reaches
/// a type's `{Name}Input` but nothing outside the request side reaches its
/// `{Name}` (the response view), the request view takes over `{Name}` and
/// every `{Name}Input` `$ref` (of `paths` and `schemas`) points at it, so no
/// orphan response view is left behind. `Tree_i32.Input` → `Tree_i32`
/// alike. A type also returned keeps both components.
///
/// The response side is everything a path reaches outside its request
/// parameters / bodies, plus every component the request side does not
/// reach (an unused type stays documented, with what it references) — not
/// the `{Name}` views being replaced, which only their own (also replaced)
/// views reference: `CreateOrder { items: Vec<LineItem> }` used only as a
/// request body gives `CreateOrder` and `LineItem`.
///
/// Runs after [`prune_unreferenced_inputs`] (every remaining input is
/// reached by a request).
pub(super) fn collapse_request_only_inputs(
    schemas: &mut BTreeMap<String, vespera_core::schema::Schema>,
    paths: &mut BTreeMap<String, vespera_core::route::PathItem>,
    input_names: &request_view::InputNames,
) -> syn::Result<()> {
    // Input component → the component whose request view it is.
    let mut inputs: BTreeMap<&str, &str> = input_names
        .iter()
        .filter(|(component, input)| {
            schemas.contains_key(component.as_str()) && schemas.contains_key(input.as_str())
        })
        .map(|(component, input)| (input.as_str(), component.as_str()))
        .collect();
    inputs.extend(
        schemas
            .keys()
            .filter_map(|name| Some((name.as_str(), name.strip_suffix(".Input")?))),
    );
    if inputs.is_empty() {
        return Ok(());
    }
    let refs = path_refs(paths);
    let request = reach(schemas, refs.request);
    let views: BTreeSet<&str> = inputs.values().copied().collect();
    let mut roots = refs.other;
    roots.extend(
        schemas
            .keys()
            .filter(|name| {
                !request.contains(*name)
                    && !views.contains(name.as_str())
                    && !inputs.contains_key(name.as_str())
            })
            .cloned(),
    );
    let response = reach(schemas, roots);
    let renames: BTreeMap<String, String> = inputs
        .into_iter()
        .filter(|(_, component)| !response.contains(*component))
        .map(|(input, component)| (input.to_string(), component.to_string()))
        .collect();
    if renames.is_empty() {
        return Ok(());
    }
    for (input, component) in &renames {
        if let Some(schema) = schemas.remove(input) {
            schemas.insert(component.clone(), schema);
        }
    }
    // A plain-data round-trip (cannot fail) through JSON text, like
    // `export_app!`'s: the text reader is already instantiated for every
    // OpenAPI type, a `from_value` one would compile a second copy.
    let rename = |text: serde_json::Result<String>| {
        text.and_then(|text| serde_json::from_str::<serde_json::Value>(&text))
            .map(|mut value| {
                crate::router_codegen::rewrite_schema_refs(&mut value, &renames);
                value.to_string()
            })
    };
    *schemas = rename(serde_json::to_string(&*schemas))
        .and_then(|text| serde_json::from_str(&text))
        .map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("vespera: failed to rename request-only input components: {error}"),
            )
        })?;
    *paths = rename(serde_json::to_string(&*paths))
        .and_then(|text| serde_json::from_str(&text))
        .map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("vespera: failed to rename request-only input paths: {error}"),
            )
        })?;
    Ok(())
}

/// Every component reachable from `pending` (its `$ref`s, transitively).
fn reach(
    schemas: &BTreeMap<String, vespera_core::schema::Schema>,
    mut pending: Vec<String>,
) -> BTreeSet<String> {
    let mut reached: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if let Some(schema) = schemas.get(&name)
            && reached.insert(name)
        {
            let value = serde_json::to_value(schema).unwrap_or_default();
            collect_component_refs(&value, &mut pending);
        }
    }
    reached
}

/// The component `$ref`s of `paths`, split by side.
struct PathRefs {
    /// Request parameters and bodies (see [`collect_path_item_refs`]).
    request: Vec<String>,
    /// Everything else (responses, callbacks, ...).
    other: Vec<String>,
}

fn path_refs(paths: &BTreeMap<String, vespera_core::route::PathItem>) -> PathRefs {
    let mut refs = PathRefs {
        request: Vec::new(),
        other: Vec::new(),
    };
    for item in paths.values() {
        // Serialization of these plain data types cannot fail.
        let value = serde_json::to_value(item).unwrap_or_default();
        collect_path_item_refs(&value, &mut refs);
    }
    refs
}

/// Component names of every `$ref` inside `value`.
fn collect_component_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref"
                    && let Some(name) = child
                        .as_str()
                        .and_then(|r| r.strip_prefix("#/components/schemas/"))
                {
                    out.push(name.to_string());
                }
                collect_component_refs(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_component_refs(child, out);
            }
        }
        _ => {}
    }
}

/// The `$ref`s of one serialized path item, split into [`PathRefs`]: the
/// request side is the path-level `parameters`, and each operation's
/// `parameters` and `requestBody` (both deserialized). Only operation-level
/// keys count: a response schema with a property named `parameters` or
/// `requestBody` is not a request.
fn collect_path_item_refs(item: &serde_json::Value, refs: &mut PathRefs) {
    const METHODS: [&str; 8] = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];
    for (key, child) in item.as_object().into_iter().flatten() {
        if key == "parameters" {
            collect_component_refs(child, &mut refs.request);
        } else if METHODS.contains(&key.as_str())
            && let Some(operation) = child.as_object()
        {
            for (op_key, op_child) in operation {
                let out = if op_key == "parameters" || op_key == "requestBody" {
                    &mut refs.request
                } else {
                    &mut refs.other
                };
                collect_component_refs(op_child, out);
            }
        }
    }
}

/// Build schema name and definition lookup maps from metadata.
///
/// Registers ALL structs (including `include_in_openapi: false`) so that
/// `schema_type!` generated types can reference them.
pub(super) fn build_schema_lookups<'a>(
    metadata: &'a CollectedMetadata,
    scoped: &'a [ScopedRustIdent<'a>],
) -> (HashSet<&'a str>, HashMap<&'a str, &'a str>) {
    let mut known_schema_names = HashSet::with_capacity(metadata.structs.len());
    let mut struct_definitions = HashMap::with_capacity(metadata.structs.len());

    for struct_meta in &metadata.structs {
        struct_definitions.insert(struct_meta.name.as_str(), struct_meta.definition.as_str());
        known_schema_names.insert(struct_meta.name.as_str());
    }
    // A type is also reachable by its module path (`crate::a::b::Foo`, or a
    // module-qualified `b::Foo`, see `parser::schema::definition_key`), so a
    // module-qualified use picks the declaration it names. A key that two
    // different declarations claim is registered for neither: the reference
    // falls back to the bare ident instead of resolving to the wrong one.
    // Declarations sharing one exact path (`cfg` twins) keep the first one by
    // schema name, as the ident alias does.
    let mut claims: BTreeMap<&str, BTreeSet<(&str, &str, &str)>> = BTreeMap::new();
    for entry in scoped {
        for key in &entry.keys {
            claims.entry(key.as_str()).or_default().insert((
                entry.keys[0].as_str(),
                entry.name,
                entry.definition,
            ));
        }
    }
    for (key, claimed) in claims {
        let paths: BTreeSet<&str> = claimed.iter().map(|(path, _, _)| *path).collect();
        if let Some((_, _, definition)) = claimed.iter().next()
            && paths.len() == 1
            && !struct_definitions.contains_key(key)
        {
            struct_definitions.insert(key, definition);
            known_schema_names.insert(key);
        }
    }
    // `#[schema(name = "X")] struct Foo` is registered as `X`, but Rust code
    // names it `Foo`: alias the Rust ident onto the same definition so every
    // use resolves (the type conversion then emits `$ref: X`, see
    // `component_name` in `parser::schema::type_schema`). A real schema of
    // the same name always wins over an alias. An ident two renamed types
    // share is resolved in schema-name order (never `metadata.structs` /
    // storage order); [`check_ambiguous_aliases`] rejects one that is used.
    for (ident, definitions) in rust_ident_aliases(metadata) {
        if !struct_definitions.contains_key(ident)
            && let Some((_, definition)) = definitions.first_key_value()
        {
            struct_definitions.insert(ident, definition);
            known_schema_names.insert(ident);
        }
    }

    (known_schema_names, struct_definitions)
}

/// A type's registry keys (see [`scoped_rust_idents`]): its exact
/// `crate::`-anchored module path first, then its module-suffix keys.
pub(super) struct ScopedRustIdent<'a> {
    keys: Vec<String>,
    name: &'a str,
    definition: &'a str,
}

/// The module-path keys of every struct whose declaring module is known from
/// its derive source identity (see [`rust_scope::crate_path`]). Renamed and
/// plain types alike: a plain `api::user::User` is what a relative `user::User`
/// is ambiguous with when a renamed `models::user::User` exists.
pub(super) fn scoped_rust_idents(metadata: &CollectedMetadata) -> Vec<ScopedRustIdent<'_>> {
    metadata
        .structs
        .iter()
        .filter_map(|meta| {
            let (modules, ident) = rust_scope::crate_path(meta.source_identity.as_deref()?)?;
            let keys = registry_keys(&modules, &ident);
            (!keys.is_empty()).then_some(ScopedRustIdent {
                keys,
                name: &meta.name,
                definition: &meta.definition,
            })
        })
        .collect()
}

/// Rust ident → (schema name → definition) of every `#[schema(name = "..")]`
/// rename, sorted on both levels so alias resolution is deterministic.
fn rust_ident_aliases(metadata: &CollectedMetadata) -> BTreeMap<&str, BTreeMap<&str, &str>> {
    let mut aliases: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for struct_meta in &metadata.structs {
        if let Some(ident) = struct_meta.rust_ident.as_deref() {
            aliases
                .entry(ident)
                .or_default()
                .insert(struct_meta.name.as_str(), struct_meta.definition.as_str());
        }
    }
    aliases
}

/// Reject a Rust type name that two `#[schema(name = "..")]` renames share
/// (`mod a { #[schema(name = "A")] struct Foo }` + `mod b { #[schema(name =
/// "B")] struct Foo }`) when a handler signature, a `responses = [...]` entry
/// or another schema uses it:
/// the macro sees only the bare ident, so it cannot tell which `Foo` is meant.
/// A schema actually NAMED like the ident wins, so it is never ambiguous.
pub(super) fn check_ambiguous_aliases(
    metadata: &CollectedMetadata,
    file_cache: &HashMap<String, syn::File>,
    route_storage: &[crate::route_impl::StoredRouteInfo],
) -> syn::Result<()> {
    let ambiguous: Vec<(&str, Vec<&str>)> = rust_ident_aliases(metadata)
        .into_iter()
        .filter(|(ident, names)| {
            names.len() > 1 && !metadata.structs.iter().any(|s| s.name == *ident)
        })
        .map(|(ident, names)| (ident, names.into_keys().collect()))
        .collect();
    if ambiguous.is_empty() {
        return Ok(());
    }
    let route_fns = metadata.routes.iter().filter_map(|route| {
        file_cache.get(&route.file_path).and_then(|ast| {
            ast.items.iter().find_map(|item| match item {
                syn::Item::Fn(f) if f.sig.ident == route.function_name => {
                    Some(quote::ToTokens::to_token_stream(&f.sig).to_string())
                }
                _ => None,
            })
        })
    });
    // `responses = [(404, Foo)]`: a one-segment type is a bare use of `Foo`
    // (a module-qualified one names its module and is resolved through it).
    let typed_responses = metadata
        .routes
        .iter()
        .flat_map(|route| route.typed_responses.iter().flatten())
        .filter(|(_, path)| !path.contains("::"))
        .map(|(_, path)| format!("fn __vespera_response() -> {path}"));
    let sources: Vec<String> = route_storage
        .iter()
        .map(|route| route.fn_sig_str.clone())
        .chain(route_fns)
        .chain(typed_responses)
        .chain(metadata.structs.iter().map(|s| s.definition.clone()))
        .collect();
    for (ident, names) in ambiguous {
        let used = sources.iter().any(|source| {
            // A definition of one of the renamed types names itself.
            !names.iter().any(|name| {
                metadata
                    .structs
                    .iter()
                    .any(|s| s.name == *name && s.definition == *source)
            }) && mentions_ident(source, ident)
        });
        if used {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "ambiguous type `{ident}`: `#[schema(name = ...)]` registers it as {} — \
                     the OpenAPI generator cannot tell which one a use of `{ident}` means; \
                     rename one of the Rust types",
                    names
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(" and "),
                ),
            ));
        }
    }
    Ok(())
}

/// Whether a type inside the item / signature `source` names `ident`
/// (attributes such as `#[derive(vespera::Schema)]` do not count).
fn mentions_ident(source: &str, ident: &str) -> bool {
    struct Finder<'a> {
        ident: &'a str,
        found: bool,
    }
    impl<'ast> syn::visit::Visit<'ast> for Finder<'_> {
        fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}
        fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
            // Only a bare `Foo` is ambiguous: a module-qualified path
            // (`super::user::Schema`, as `schema_type!` relations emit)
            // names its module and is resolved through it.
            self.found |= path.qself.is_none()
                && path
                    .path
                    .get_ident()
                    .is_some_and(|found| found == self.ident);
            syn::visit::visit_type_path(self, path);
        }
    }
    let mut finder = Finder {
        ident,
        found: false,
    };
    if let Ok(item) = syn::parse_str::<syn::Item>(source) {
        syn::visit::Visit::visit_item(&mut finder, &item);
    } else if let Ok(sig) = syn::parse_str::<syn::Signature>(source) {
        syn::visit::Visit::visit_signature(&mut finder, &sig);
    }
    finder.found
}

/// Build file AST cache — parse each unique route file exactly once.
///
/// Deduplicates file paths first, then parses each file a single time.
/// This eliminates redundant file I/O when multiple routes share a source file.
pub(super) fn build_file_cache(metadata: &CollectedMetadata) -> HashMap<String, syn::File> {
    let unique_paths: BTreeSet<&str> = metadata
        .routes
        .iter()
        .map(|r| r.file_path.as_str())
        .collect();
    let mut cache = HashMap::with_capacity(unique_paths.len());
    for path in unique_paths {
        if let Some(ast) = crate::schema_macro::file_cache::get_parsed_file(Path::new(path)) {
            cache.insert(path.to_string(), ast);
        }
    }
    cache
}

/// Build struct name → file path index from cached file ASTs.
///
/// Enables O(1) lookup of which file contains a given struct definition,
/// replacing the previous O(routes × file_read) linear scan.
pub(super) fn build_struct_file_index(
    file_cache: &HashMap<String, syn::File>,
) -> HashMap<String, &str> {
    let mut index = HashMap::with_capacity(file_cache.len() * 4);
    // Path order (not `HashMap` order) decides which file a struct declared
    // in several route files resolves to, so the result is deterministic.
    let mut files: Vec<(&String, &syn::File)> = file_cache.iter().collect();
    files.sort_unstable_by_key(|(path, _)| *path);
    for (path, ast) in files {
        for item in &ast.items {
            if let syn::Item::Struct(s) = item {
                index.entry(s.ident.to_string()).or_insert(path.as_str());
            }
        }
    }
    index
}

/// The file AST a struct's Priority-2 (`#[serde(default = "fn")]`) functions
/// resolve in.
///
/// The file its own `source_identity` names is the one that declares it. An
/// ident-only lookup would read a same-named function from another file: a
/// `struct User` in two route files, or a renamed `#[schema(name)]` struct whose
/// ident is shared, resolves to whichever file sorts first. The ident index is
/// used only for a struct without a source identity.
fn struct_default_file<'a>(
    struct_meta: &crate::metadata::StructMetadata,
    file_cache: &'a HashMap<String, syn::File>,
    struct_file_index: &HashMap<String, &str>,
) -> Option<&'a syn::File> {
    if let Some(identity) = struct_meta.source_identity.as_deref() {
        return identity_file(identity).and_then(|path| file_for_path(file_cache, path));
    }
    struct_file_index
        .get(&struct_meta.name)
        .or_else(|| {
            struct_meta
                .rust_ident
                .as_ref()
                .and_then(|ident| struct_file_index.get(ident))
        })
        .and_then(|path| file_cache.get(*path))
}

/// The file part of a `source_identity` (`file::Ident@line:col`).
fn identity_file(identity: &str) -> Option<&str> {
    let (base, _position) = identity.rsplit_once('@')?;
    base.rsplit_once("::").map(|(file, _ident)| file)
}

/// The cached file AST at `path`. The identity spells its file as the
/// compiler reported it (cwd-relative, `\` on Windows, `.` components), while
/// `file_cache` is keyed by the collector's normalized route paths, so an
/// exact miss falls back to the entry with the same comparison key
/// ([`normalize_path_key`]). The exact lookup stays the fast path.
fn file_for_path<'a>(
    file_cache: &'a HashMap<String, syn::File>,
    path: &str,
) -> Option<&'a syn::File> {
    if let Some(ast) = file_cache.get(path) {
        return Some(ast);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let wanted = crate::file_utils::normalize_path_key(path, &cwd);
    file_cache
        .iter()
        .find(|(key, _)| crate::file_utils::normalize_path_key(key, &cwd) == wanted)
        .map(|(_, ast)| ast)
}

/// Parse struct and enum definitions into `OpenAPI` component schemas.
///
/// Only includes structs where `include_in_openapi` is true
/// (i.e., from `#[derive(Schema)]`, not from cross-file lookup).
/// Also processes `#[serde(default)]` attributes to extract default values.
///
/// Uses pre-built `file_cache` and `struct_file_index` for O(1) file lookups
/// instead of scanning all route files per struct.
pub(super) fn parse_component_schemas(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
    file_cache: &HashMap<String, syn::File>,
    struct_file_index: &HashMap<String, &str>,
) -> syn::Result<BTreeMap<String, vespera_core::schema::Schema>> {
    // Parse a definition string and build its schema, applying the
    // default-value pipeline.  `file_ast` is only needed for the
    // `#[serde(default = "fn_name")]` fallback (Priority 2) — the
    // pre-extracted SCHEMA_STORAGE defaults, `#[schema(default)]`
    // attributes, and type defaults apply even without an AST (the
    // collector fast path skips parsing, leaving `file_cache` empty).
    let build_one = |struct_meta: &crate::metadata::StructMetadata,
                     file_ast: Option<&syn::File>|
     -> syn::Result<Option<Vec<(String, vespera_core::schema::Schema)>>> {
        let parsed = syn::parse_str::<syn::Item>(&struct_meta.definition).map_err(|err| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "failed to parse component schema `{}` metadata: {err}",
                    struct_meta.name
                ),
            )
        })?;
        // A renamed type naming its own Rust ident (`children: Vec<Node>`)
        // means itself, not whichever same-ident rename the shared alias
        // resolves to (`mod a` / `mod b` both `Node`).
        let own_definitions: HashMap<&str, &str>;
        let struct_definitions = match struct_meta.rust_ident.as_deref() {
            Some(ident)
                if struct_definitions
                    .get(ident)
                    .is_some_and(|definition| *definition != struct_meta.definition) =>
            {
                own_definitions = struct_definitions
                    .iter()
                    .map(|(key, definition)| (*key, *definition))
                    .chain([(ident, struct_meta.definition.as_str())])
                    .collect();
                &own_definitions
            }
            _ => struct_definitions,
        };
        // One view of the item: the response view, or (inside
        // `with_request_view`) the request view with deserialize names.
        let build_view = |side: SerdeSide| {
            let mut schema = match &parsed {
                syn::Item::Struct(struct_item) => {
                    parse_struct_to_schema(struct_item, known_schema_names, struct_definitions)
                }
                syn::Item::Enum(enum_item) => {
                    parse_enum_to_schema(enum_item, known_schema_names, struct_definitions)
                }
                _ => return None,
            };
            if let syn::Item::Struct(struct_item) = &parsed {
                process_default_functions_for(
                    struct_item,
                    file_ast,
                    &mut schema,
                    &struct_meta.field_defaults,
                    side,
                );
            }
            Some(schema)
        };
        let Some(schema) = build_view(SerdeSide::Serialize) else {
            return Ok(None);
        };
        let mut views = vec![(struct_meta.name.clone(), schema)];
        // The `{Name}Input` request view, when it differs.
        if let Some(input_name) = request_view::input_name(&struct_meta.name)
            && let Some(input) =
                request_view::with_request_view(|| build_view(SerdeSide::Deserialize))
        {
            views.push((input_name, input));
        }
        Ok(Some(views))
    };

    // Partition: structs whose file AST is reachable need the
    // (non-`Send`) AST for Priority-2 default extraction and run on
    // this thread; everything else parses + builds on workers
    // returning plain `Schema` data.
    let mut ast_backed: Vec<(&crate::metadata::StructMetadata, &syn::File)> = Vec::new();
    let mut parallel_jobs: Vec<&crate::metadata::StructMetadata> = Vec::new();
    for struct_meta in metadata.structs.iter().filter(|s| s.include_in_openapi) {
        // Use ONLY the struct's own indexed file AST for Priority-2
        // (`#[serde(default = "fn")]`) default extraction.  The former
        // fallback to `metadata.routes.first()`'s AST could resolve a
        // same-named default fn from an UNRELATED route file and emit a
        // wrong OpenAPI default; a struct whose file is not indexed now
        // simply forgoes Priority-2 extraction (other default sources still
        // apply) rather than risking an incorrect value.
        let file_ast = struct_default_file(struct_meta, file_cache, struct_file_index);
        match file_ast {
            Some(ast) => ast_backed.push((struct_meta, ast)),
            None => parallel_jobs.push(struct_meta),
        }
    }

    let mut schemas = BTreeMap::new();
    for views in parallel_filter_map(
        &parallel_jobs,
        &|meta: &&crate::metadata::StructMetadata| build_one(meta, None),
    )? {
        schemas.extend(views);
    }
    for (struct_meta, ast) in ast_backed {
        if let Some(views) = build_one(struct_meta, Some(ast))? {
            schemas.extend(views);
        }
    }

    Ok(schemas)
}

/// [`reject_internally_tagged_non_object_refs`] over the finished document:
/// every component — generic instances (`Event_String`) are only
/// materialized while the paths are built — and every inline request /
/// response schema of `paths`.
pub(super) fn reject_internally_tagged_in_document(
    schemas: &BTreeMap<String, vespera_core::schema::Schema>,
    paths: &BTreeMap<String, vespera_core::route::PathItem>,
) -> syn::Result<()> {
    reject_internally_tagged_non_object_refs(schemas)?;
    for (path, item) in paths {
        for (method, operation) in item.operations() {
            let request = operation
                .request_body
                .iter()
                .flat_map(|body| body.content.values());
            let responses = operation
                .responses
                .values()
                .filter_map(|response| response.content.as_ref())
                .flat_map(BTreeMap::values);
            for media in request.chain(responses) {
                if let Some(vespera_core::schema::SchemaRef::Inline(schema)) = &media.schema {
                    check_internally_tagged(&format!("{method} {path}"), schema, schemas)?;
                }
            }
        }
    }
    Ok(())
}

/// Reject an internally tagged enum newtype variant that wraps a `$ref` to a
/// component known to serialize as a non-object (e.g. `struct UserId(String)`
/// emitting a `string` component).  `#[derive(Schema)]` only sees the variant's
/// type NAME, so this can only be decided once every component is built; a
/// `$ref` to a component not in this spec (external / merged) is left alone.
///
/// Internally tagged newtype variants are the only `oneOf` branches emitted as
/// `allOf: [{tag object}, {$ref}]` under a `discriminator`.
fn reject_internally_tagged_non_object_refs(
    schemas: &BTreeMap<String, vespera_core::schema::Schema>,
) -> syn::Result<()> {
    for (enum_name, schema) in schemas {
        check_internally_tagged(enum_name, schema, schemas)?;
    }
    Ok(())
}

/// The internally tagged non-object check of one schema named `enum_name`.
fn check_internally_tagged(
    enum_name: &str,
    schema: &vespera_core::schema::Schema,
    schemas: &BTreeMap<String, vespera_core::schema::Schema>,
) -> syn::Result<()> {
    use vespera_core::schema::{SchemaRef, SchemaType};
    let Some(discriminator) = &schema.discriminator else {
        return Ok(());
    };
    let tag = &discriminator.property_name;
    for branch in schema.one_of.iter().flatten() {
        let SchemaRef::Inline(branch) = branch else {
            continue;
        };
        let Some([SchemaRef::Inline(tag_object), payload]) = branch.all_of.as_deref() else {
            continue;
        };
        let non_object = |schema: &vespera_core::schema::Schema| {
            schema
                .schema_type
                .is_some_and(|ty| !matches!(ty, SchemaType::Object | SchemaType::Null))
        };
        // An inline payload (`Uuid`, `NaiveDate`, ...) is named by its
        // wire type.
        let target = match payload {
            SchemaRef::Ref(reference) => {
                let Some(target) = reference.ref_path.strip_prefix("#/components/schemas/") else {
                    continue;
                };
                if !schemas.get(target).is_some_and(non_object) {
                    continue;
                }
                target.to_string()
            }
            SchemaRef::Inline(inline) if non_object(inline) => {
                let ty = format!("{:?}", inline.schema_type.unwrap_or(SchemaType::Null));
                inline.format.as_ref().map_or_else(
                    || ty.to_lowercase(),
                    |format| format!("{} ({format})", ty.to_lowercase()),
                )
            }
            SchemaRef::Inline(_) => continue,
        };
        let variant = tag_object
            .properties
            .as_ref()
            .and_then(|properties| properties.get(tag))
            .and_then(|tag_schema| match tag_schema {
                SchemaRef::Inline(tag_schema) => tag_schema.r#enum.as_ref()?.first()?.as_str(),
                SchemaRef::Ref(_) => None,
            })
            .unwrap_or("?");
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "`{enum_name}`: {}",
                crate::parser::schema::internally_tagged_non_object_message(tag, variant, &target)
            ),
        ));
    }
    Ok(())
}

mod rust_scope;

#[cfg(test)]
mod tests;
