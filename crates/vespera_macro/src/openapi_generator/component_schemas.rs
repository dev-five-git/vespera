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
        schema::{SerdeSide, request_view},
    },
};

/// The `{Name}Input` request-view names for the documented types (see
/// [`request_view`]). Without any `serde` attribute no view can differ, so
/// the definitions are not even parsed.
pub(super) fn input_names(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<&str>,
) -> request_view::InputNames {
    let documented: Vec<&crate::metadata::StructMetadata> = metadata
        .structs
        .iter()
        .filter(|s| s.include_in_openapi)
        .collect();
    if !documented.iter().any(|s| s.definition.contains("serde")) {
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
        input_names
            .iter()
            .filter(|(_, input)| !identical.contains(input))
            .map(|(component, input)| (component.clone(), input.clone()))
            .collect(),
    ))
}

/// Drop the `{Name}Input` components no request body reaches: a type that
/// only ever appears in responses keeps a single component.
pub(super) fn prune_unreferenced_inputs(
    schemas: &mut BTreeMap<String, vespera_core::schema::Schema>,
    paths: &BTreeMap<String, vespera_core::route::PathItem>,
    input_names: &request_view::InputNames,
) {
    let inputs: BTreeSet<&str> = input_names.values().map(String::as_str).collect();
    if inputs.is_empty() {
        return;
    }
    // Every component reachable from a request body.
    let mut pending: Vec<String> = Vec::new();
    for item in paths.values() {
        // Serialization of these plain data types cannot fail.
        let value = serde_json::to_value(item).unwrap_or_default();
        collect_request_body_refs(&value, false, &mut pending);
    }
    let mut reached: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if let Some(schema) = schemas.get(&name)
            && reached.insert(name)
        {
            let value = serde_json::to_value(schema).unwrap_or_default();
            collect_request_body_refs(&value, true, &mut pending);
        }
    }
    schemas.retain(|name, _| !inputs.contains(name.as_str()) || reached.contains(name));
}

/// Component names of the `$ref`s inside `value`, counting only those under
/// a `requestBody` unless `inside` is already set.
fn collect_request_body_refs(value: &serde_json::Value, inside: bool, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if inside
                    && key == "$ref"
                    && let Some(name) = child
                        .as_str()
                        .and_then(|r| r.strip_prefix("#/components/schemas/"))
                {
                    out.push(name.to_string());
                }
                collect_request_body_refs(child, inside || key == "requestBody", out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_request_body_refs(child, inside, out);
            }
        }
        _ => {}
    }
}

/// Build schema name and definition lookup maps from metadata.
///
/// Registers ALL structs (including `include_in_openapi: false`) so that
/// `schema_type!` generated types can reference them.
pub(super) fn build_schema_lookups(
    metadata: &CollectedMetadata,
) -> (HashSet<&str>, HashMap<&str, &str>) {
    let mut known_schema_names = HashSet::with_capacity(metadata.structs.len());
    let mut struct_definitions = HashMap::with_capacity(metadata.structs.len());

    for struct_meta in &metadata.structs {
        struct_definitions.insert(struct_meta.name.as_str(), struct_meta.definition.as_str());
        known_schema_names.insert(struct_meta.name.as_str());
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
/// "B")] struct Foo }`) when a handler signature or another schema uses it:
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
    let sources: Vec<String> = route_storage
        .iter()
        .map(|route| route.fn_sig_str.clone())
        .chain(route_fns)
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
        let file_ast = struct_file_index
            .get(&struct_meta.name)
            .or_else(|| {
                struct_meta
                    .rust_ident
                    .as_ref()
                    .and_then(|ident| struct_file_index.get(ident))
            })
            .and_then(|path| file_cache.get(*path));
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

    reject_internally_tagged_non_object_refs(&schemas)?;
    Ok(schemas)
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
    use vespera_core::schema::{SchemaRef, SchemaType};
    for (enum_name, schema) in schemas {
        let Some(discriminator) = &schema.discriminator else {
            continue;
        };
        let tag = &discriminator.property_name;
        for branch in schema.one_of.iter().flatten() {
            let SchemaRef::Inline(branch) = branch else {
                continue;
            };
            let Some([SchemaRef::Inline(tag_object), SchemaRef::Ref(reference)]) =
                branch.all_of.as_deref()
            else {
                continue;
            };
            let Some(target) = reference.ref_path.strip_prefix("#/components/schemas/") else {
                continue;
            };
            let non_object = schemas.get(target).is_some_and(|target| {
                target
                    .schema_type
                    .is_some_and(|ty| !matches!(ty, SchemaType::Object | SchemaType::Null))
            });
            if !non_object {
                continue;
            }
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
                    crate::parser::schema::internally_tagged_non_object_message(
                        tag, variant, target
                    )
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
