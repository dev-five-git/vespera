//! B2: compile-time validation that request/query extractors reference
//! `Schema`-backed types.
//!
//! `Query<T>`, `Json<T>`, `Form<T>`, and `TypedMultipart<T>` only appear in the
//! generated OpenAPI when `T` is known to Vespera (i.e. it derives `Schema`).
//! When `T` is a struct declared in the same route file that does **not** derive
//! `Schema`, Vespera silently drops it — `Query<T>` yields no parameters and
//! `Json<T>` falls back to a generic object — so the spec lies about the route.
//!
//! This pass turns that silent footgun into a hard compile error, scoped to the
//! one case the macro can prove: a struct **declared in the handler's own file**
//! that is absent from `known_schema_names`. Primitives, containers, maps,
//! external/imported types, and `Schema`-deriving structs are never flagged —
//! the macro cannot prove `Schema` for types it cannot name-resolve, and a false
//! positive there would be worse than the residual (cross-file) false negative.

use std::collections::{HashMap, HashSet};

use proc_macro2::Span;
use syn::Type;

use super::extractors::unwrap_validated_type;
use crate::metadata::CollectedMetadata;

/// Request/query extractors whose generic argument must be a documented type.
const REQUEST_EXTRACTORS: [&str; 4] = ["Query", "Json", "Form", "TypedMultipart"];

/// Validate every route handler's request/query extractors against the set of
/// `Schema`-backed type names. Returns a `compile_error!`-ready `syn::Error` on
/// the first same-file non-`Schema` struct used in such an extractor.
///
/// Only call sites with a parsed file AST (cache-miss / `export_app!`) run this;
/// a cache hit means the source is byte-identical to a build that already
/// passed, so re-validation is unnecessary.
/// Validate schema-backed extractors using an invocation-local AST cache
/// already produced by route collection.
pub fn validate_schema_backed_extractors_with_cache(
    metadata: &CollectedMetadata,
    file_cache: &HashMap<String, syn::File>,
) -> syn::Result<()> {
    check_extractors(metadata, file_cache)
}

fn check_extractors(
    metadata: &CollectedMetadata,
    file_cache: &HashMap<String, syn::File>,
) -> syn::Result<()> {
    let known: HashSet<&str> = metadata.structs.iter().map(|s| s.name.as_str()).collect();
    // Map each route file's module path → its file path so an absolute
    // `crate::<route-module>::Type` import can be resolved back to the route file
    // and checked: a path that resolves *inside* the route folder names a route
    // type, while `crate::models::…` (outside the folder) is not in this map and
    // stays skipped.
    let route_module_files: HashMap<&str, &str> = metadata
        .routes
        .iter()
        .map(|r| (r.module_path.as_str(), r.file_path.as_str()))
        .collect();

    // Per-file analysis cache: the local type set and the imported non-`Schema`
    // route-type set depend only on the file (every route in a file shares one
    // module path), so compute them ONCE per file and reuse them for every
    // route in that file. The previous code recomputed both per route — scanning
    // the file's items and re-resolving its imports `routes_in_file` times
    // (O(routes_in_file x items_in_file) on every cache-miss build). Routes are
    // still visited in declaration order, so the first reported violation is
    // deterministic.
    let mut file_analysis: HashMap<&str, (HashSet<String>, HashSet<String>)> = HashMap::new();

    for route in &metadata.routes {
        let Some(ast) = file_cache.get(&route.file_path) else {
            continue;
        };

        let (local_types, imported_route_types) = &*file_analysis
            .entry(route.file_path.as_str())
            .or_insert_with(|| {
                // Types physically declared in this route file (structs + enums).
                let local_types: HashSet<String> = ast
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        syn::Item::Struct(s) => Some(s.ident.to_string()),
                        syn::Item::Enum(e) => Some(e.ident.to_string()),
                        _ => None,
                    })
                    .collect();
                // Non-`Schema` types imported from another route file via a
                // `crate`/`self`/`super` path (resolved against this file's module).
                let mut imported_route_types = HashSet::new();
                collect_imported_route_types(
                    ast,
                    &route.module_path,
                    &route_module_files,
                    file_cache,
                    &known,
                    &mut imported_route_types,
                );
                (local_types, imported_route_types)
            });

        let Some(fn_item) = ast.items.iter().find_map(|item| match item {
            syn::Item::Fn(f) if f.sig.ident == route.function_name => Some(f),
            _ => None,
        }) else {
            continue;
        };

        for input in &fn_item.sig.inputs {
            let syn::FnArg::Typed(syn::PatType { ty, .. }) = input else {
                continue;
            };
            let unwrapped = unwrap_validated_type(ty.as_ref());
            let Some((extractor, inner)) = request_extractor_inner(unwrapped) else {
                continue;
            };

            let mut idents = Vec::new();
            collect_custom_type_idents(inner, &mut idents);
            for ident in idents {
                let local_without_schema =
                    local_types.contains(&ident) && !known.contains(ident.as_str());
                if local_without_schema || imported_route_types.contains(&ident) {
                    return Err(syn::Error::new(
                        Span::call_site(),
                        format!(
                            "vespera! macro: route `{fn_name}` uses `{extractor}<{ident}>`, but \
                             `{ident}` does not derive `Schema`. Vespera cannot document a \
                             non-`Schema` type and would silently drop it from the OpenAPI spec. \
                             Add `#[derive(vespera::Schema)]` to `{ident}`.",
                            fn_name = route.function_name,
                        ),
                    ));
                }
            }
        }
    }

    Ok(())
}

/// If `ty` is one of the request/query extractors, return its name and the
/// first generic type argument.
fn request_extractor_inner(ty: &Type) -> Option<(&'static str, &Type)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    let extractor = REQUEST_EXTRACTORS
        .into_iter()
        .find(|name| segment.ident == name)?;
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let syn::GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    Some((extractor, inner))
}

/// Collect the last path-segment identifier of `ty` and recurse through generic
/// arguments and references. Container idents (`Vec`, `Option`, ...) and
/// primitives are harmlessly collected too — they are filtered out later by the
/// `local_types` / imported-route-type membership test, so no explicit
/// allow/deny list is needed.
fn collect_custom_type_idents(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                out.push(segment.ident.to_string());
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    for arg in &args.args {
                        if let syn::GenericArgument::Type(inner) = arg {
                            collect_custom_type_idents(inner, out);
                        }
                    }
                }
            }
        }
        Type::Reference(reference) => collect_custom_type_idents(&reference.elem, out),
        _ => {}
    }
}

/// Collect the in-scope idents of every `use` import that resolves, inside the
/// route folder, to a route file declaring a non-`Schema` struct/enum — the
/// cross-file footgun. `crate`, `self`, and `super` (any depth) prefixes are
/// resolved against `current_module` (the importing file's own module path);
/// imports that climb above the crate root, land outside the route folder (not
/// in `route_module_files`), or whose *declared* type derives `Schema` are left
/// untouched — so aliasing (`as`) never produces a false positive.
///
/// Residual: a type declared in a route-folder file that has no `#[route]`
/// handler is absent from `route_module_files`, so such an import is not flagged
/// (a safe false negative, never a false positive).
fn collect_imported_route_types(
    ast: &syn::File,
    current_module: &str,
    route_module_files: &HashMap<&str, &str>,
    file_cache: &HashMap<String, syn::File>,
    known: &HashSet<&str>,
    out: &mut HashSet<String>,
) {
    let current: Vec<&str> = current_module.split("::").collect();
    for item in &ast.items {
        if let syn::Item::Use(item_use) = item
            && let Some((mut base, rest)) = resolve_use_prefix(&item_use.tree, &current)
        {
            walk_module_path(rest, &mut base, route_module_files, file_cache, known, out);
        }
    }
}

/// Resolve a use-tree's leading `crate`/`self`/`super…` prefix into the base
/// module-path segments and the remaining subtree. Returns `None` for external
/// crates, bare items, or `super` chains that climb above the crate root.
fn resolve_use_prefix<'a>(
    tree: &'a syn::UseTree,
    current: &[&str],
) -> Option<(Vec<String>, &'a syn::UseTree)> {
    let syn::UseTree::Path(first) = tree else {
        return None;
    };
    match first.ident.to_string().as_str() {
        "crate" => Some((Vec::new(), first.tree.as_ref())),
        "self" => Some((
            current.iter().map(|s| (*s).to_string()).collect(),
            first.tree.as_ref(),
        )),
        "super" => {
            let mut supers = 1usize;
            let mut node: &syn::UseTree = first.tree.as_ref();
            while let syn::UseTree::Path(next) = node {
                if next.ident == "super" {
                    supers += 1;
                    node = next.tree.as_ref();
                } else {
                    break;
                }
            }
            let kept = current.len().checked_sub(supers)?;
            Some((
                current[..kept].iter().map(|s| (*s).to_string()).collect(),
                node,
            ))
        }
        _ => None,
    }
}

/// Walk the post-prefix subtree, accumulating module segments, and record every
/// leaf import naming a non-`Schema` type declared in a resolved route file.
fn walk_module_path(
    tree: &syn::UseTree,
    module_segments: &mut Vec<String>,
    route_module_files: &HashMap<&str, &str>,
    file_cache: &HashMap<String, syn::File>,
    known: &HashSet<&str>,
    out: &mut HashSet<String>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            module_segments.push(path.ident.to_string());
            walk_module_path(
                &path.tree,
                module_segments,
                route_module_files,
                file_cache,
                known,
                out,
            );
            module_segments.pop();
        }
        syn::UseTree::Name(name) => {
            record_route_type(
                module_segments,
                &name.ident,
                &name.ident,
                route_module_files,
                file_cache,
                known,
                out,
            );
        }
        syn::UseTree::Rename(rename) => {
            // The alias (`rename`) is the in-scope name used in handler
            // signatures; the original (`ident`) is what the source module
            // declares and what determines `Schema` status.
            record_route_type(
                module_segments,
                &rename.ident,
                &rename.rename,
                route_module_files,
                file_cache,
                known,
                out,
            );
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                walk_module_path(
                    item,
                    module_segments,
                    route_module_files,
                    file_cache,
                    known,
                    out,
                );
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// Record `bound` (the in-scope name) when `module_segments` resolves to a route
/// file that declares a struct/enum named `declared` which does not derive
/// `Schema`. The `Schema` check uses the *declared* name, so aliasing a
/// `Schema`-deriving type (`use … as X`) never produces a false positive.
fn record_route_type(
    module_segments: &[String],
    declared: &syn::Ident,
    bound: &syn::Ident,
    route_module_files: &HashMap<&str, &str>,
    file_cache: &HashMap<String, syn::File>,
    known: &HashSet<&str>,
    out: &mut HashSet<String>,
) {
    if known.contains(declared.to_string().as_str()) {
        return;
    }
    let module = module_segments.join("::");
    if let Some(&file_path) = route_module_files.get(module.as_str())
        && let Some(file_ast) = file_cache.get(file_path)
        && file_declares_type(file_ast, declared)
    {
        out.insert(bound.to_string());
    }
}

/// Whether `ast` declares a struct or enum named `ident`.
fn file_declares_type(ast: &syn::File, ident: &syn::Ident) -> bool {
    ast.items.iter().any(|item| match item {
        syn::Item::Struct(s) => s.ident == *ident,
        syn::Item::Enum(e) => e.ident == *ident,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{CollectedMetadata, RouteMetadata, StructMetadata};

    fn route(function_name: &str, file_path: &str) -> RouteMetadata {
        RouteMetadata {
            method: "get".to_string(),
            path: "/x".to_string(),
            function_name: function_name.to_string(),
            module_path: "routes::x".to_string(),
            file_path: file_path.to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        }
    }

    fn run(src: &str, fn_name: &str, structs: &[&str]) -> syn::Result<()> {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route(fn_name, "f.rs"));
        for name in structs {
            metadata
                .structs
                .push(StructMetadata::new((*name).to_string(), String::new()));
        }
        let ast: syn::File = syn::parse_str(src).expect("source parses");
        let mut file_cache = HashMap::new();
        file_cache.insert("f.rs".to_string(), ast);
        check_extractors(&metadata, &file_cache)
    }

    #[test]
    fn local_struct_without_schema_in_query_errors() {
        let src = r"
            pub struct Local { pub a: i32 }
            pub fn handler(Query(q): Query<Local>) -> String { String::new() }
        ";
        let err = run(src, "handler", &[]).expect_err("should error");
        let msg = err.to_string();
        assert!(msg.contains("Local"), "got: {msg}");
        assert!(msg.contains("Query<Local>"), "got: {msg}");
        assert!(msg.contains("does not derive `Schema`"), "got: {msg}");
    }

    #[test]
    fn local_struct_with_schema_is_ok() {
        // `Local` present in metadata.structs ⇒ it derived Schema.
        let src = r"
            pub struct Local { pub a: i32 }
            pub fn handler(Query(q): Query<Local>) -> String { String::new() }
        ";
        assert!(run(src, "handler", &["Local"]).is_ok());
    }

    #[test]
    fn external_non_local_type_is_not_flagged() {
        // `External` is not declared as a struct in this file ⇒ skipped.
        let src = r"
            pub fn handler(Query(q): Query<External>) -> String { String::new() }
        ";
        assert!(run(src, "handler", &[]).is_ok());
    }

    #[test]
    fn validated_json_unwraps_and_flags_inner_local_struct() {
        let src = r"
            pub struct Local { pub a: i32 }
            pub fn handler(Validated(Json(b)): Validated<Json<Local>>) -> String { String::new() }
        ";
        let err = run(src, "handler", &[]).expect_err("should error");
        assert!(err.to_string().contains("Json<Local>"), "{err}");
    }

    #[test]
    fn nested_container_inner_local_struct_is_flagged() {
        let src = r"
            pub struct Local { pub a: i32 }
            pub fn handler(Json(b): Json<Vec<Local>>) -> String { String::new() }
        ";
        assert!(run(src, "handler", &[]).is_err());
    }

    #[test]
    fn primitive_query_is_ok() {
        let src = r"
            pub fn handler(Query(q): Query<String>) -> String { String::new() }
        ";
        assert!(run(src, "handler", &[]).is_ok());
    }

    #[test]
    fn same_file_enum_without_schema_is_flagged() {
        // Same-file enums are documentable too — a non-`Schema` enum in a body
        // extractor is the same footgun as a struct.
        let src = r"
            pub enum Kind { A, B }
            pub fn handler(Json(b): Json<Kind>) -> String { String::new() }
        ";
        let err = run(src, "handler", &[]).expect_err("should error");
        assert!(err.to_string().contains("Kind"), "{err}");
    }

    #[test]
    fn relative_super_import_non_schema_type_is_flagged() {
        // `use super::other::Bar` from `routes::handler` resolves to sibling route
        // file `other` (`super` → `routes`); lacking Schema it must be flagged.
        let err = run_with_route_sibling(
            "use super::other::Bar; pub fn handler(Json(b): Json<Bar>) -> String { String::new() }",
            "routes::other",
            "pub struct Bar { pub a: i32 }",
            &[],
        )
        .expect_err("should flag relative route import");
        assert!(err.to_string().contains("Bar"), "{err}");
    }

    #[test]
    fn relative_super_import_schema_type_is_ok() {
        // Same relative import, but `Bar` derives Schema (∈ known) ⇒ not flagged.
        assert!(
            run_with_route_sibling(
                "use super::other::Bar; pub fn handler(Json(b): Json<Bar>) -> String { String::new() }",
                "routes::other",
                "pub struct Bar { pub a: i32 }",
                &["Bar"],
            )
            .is_ok()
        );
    }

    #[test]
    fn absolute_crate_import_outside_routes_is_not_flagged() {
        // `crate::models::…` resolves outside the route folder, so it is not in
        // the route module map → conservatively skipped (no false positive).
        let src = r"
            use crate::models::Bar;
            pub fn handler(Json(b): Json<Bar>) -> String { String::new() }
        ";
        assert!(run(src, "handler", &[]).is_ok());
    }

    /// Two-file metadata: a `handler` route plus a sibling route module `other`.
    fn run_with_route_sibling(
        handler_src: &str,
        sibling_module: &str,
        sibling_src: &str,
        known: &[&str],
    ) -> syn::Result<()> {
        let mut metadata = CollectedMetadata::new();
        let mut handler = route("handler", "handler.rs");
        handler.module_path = "routes::handler".to_string();
        metadata.routes.push(handler);
        let sibling_file = format!("{}.rs", sibling_module.rsplit("::").next().unwrap());
        let mut sibling = route("sibling", &sibling_file);
        sibling.module_path = sibling_module.to_string();
        metadata.routes.push(sibling);
        for name in known {
            metadata
                .structs
                .push(StructMetadata::new((*name).to_string(), String::new()));
        }
        let mut file_cache = HashMap::new();
        file_cache.insert(
            "handler.rs".to_string(),
            syn::parse_str(handler_src).expect("handler parses"),
        );
        file_cache.insert(
            sibling_file,
            syn::parse_str(sibling_src).expect("sibling parses"),
        );
        check_extractors(&metadata, &file_cache)
    }

    #[test]
    fn absolute_crate_import_into_routes_is_flagged() {
        // `use crate::routes::other::Bar` resolves to the route file `other`,
        // which declares a non-Schema `Bar` → flagged despite the absolute path.
        let err = run_with_route_sibling(
            "use crate::routes::other::Bar; pub fn handler(Json(b): Json<Bar>) -> String { String::new() }",
            "routes::other",
            "pub struct Bar { pub a: i32 }",
            &[],
        )
        .expect_err("should flag absolute route import");
        assert!(err.to_string().contains("Bar"), "{err}");
    }

    #[test]
    fn absolute_crate_import_into_routes_with_schema_is_ok() {
        // Same absolute import, but `Bar` derives Schema (∈ known) → not flagged.
        assert!(
            run_with_route_sibling(
                "use crate::routes::other::Bar; pub fn handler(Json(b): Json<Bar>) -> String { String::new() }",
                "routes::other",
                "pub struct Bar { pub a: i32 }",
                &["Bar"],
            )
            .is_ok()
        );
    }

    #[test]
    fn absolute_crate_import_to_non_type_is_not_flagged() {
        // The sibling route module exists but declares no `Bar` type (only a
        // re-export / fn) → `file_declares_type` is false → not flagged.
        assert!(
            run_with_route_sibling(
                "use crate::routes::other::Bar; pub fn handler(Json(b): Json<Bar>) -> String { String::new() }",
                "routes::other",
                "pub fn helper() {}",
                &[],
            )
            .is_ok()
        );
    }

    #[test]
    fn aliased_schema_type_import_is_not_flagged() {
        // Aliasing a Schema-deriving type (`use … as X`) must NOT be flagged: the
        // Schema check uses the declared name, not the alias. (Regression for the
        // alias false positive.)
        assert!(
            run_with_route_sibling(
                "use crate::routes::other::Bar as B; pub fn handler(Query(q): Query<B>) -> String { String::new() }",
                "routes::other",
                "pub struct Bar { pub a: i32 }",
                &["Bar"],
            )
            .is_ok()
        );
    }

    #[test]
    fn aliased_non_schema_type_import_is_flagged() {
        // Aliasing a non-Schema route type is still flagged, under the alias name.
        let err = run_with_route_sibling(
            "use crate::routes::other::Bar as B; pub fn handler(Query(q): Query<B>) -> String { String::new() }",
            "routes::other",
            "pub struct Bar { pub a: i32 }",
            &[],
        )
        .expect_err("should flag aliased non-Schema import");
        assert!(err.to_string().contains('B'), "{err}");
    }

    #[test]
    fn multi_super_into_routes_is_flagged() {
        // From a nested module, `super::super` rises to `routes`, so
        // `super::super::other::Bar` resolves to the route file `other`.
        let mut metadata = CollectedMetadata::new();
        let mut handler = route("handler", "stats.rs");
        handler.module_path = "routes::admin::stats".to_string();
        metadata.routes.push(handler);
        let mut other = route("other_handler", "other.rs");
        other.module_path = "routes::other".to_string();
        metadata.routes.push(other);

        let mut file_cache = HashMap::new();
        file_cache.insert(
            "stats.rs".to_string(),
            syn::parse_str(
                "use super::super::other::Bar; pub fn handler(Json(b): Json<Bar>) -> String { String::new() }",
            )
            .unwrap(),
        );
        file_cache.insert(
            "other.rs".to_string(),
            syn::parse_str("pub struct Bar { pub a: i32 }").unwrap(),
        );

        assert!(check_extractors(&metadata, &file_cache).is_err());
    }

    #[test]
    fn multi_super_escaping_routes_is_not_flagged() {
        // `super::super` from a top-level route file rises to the crate root, so
        // `super::super::models::Bar` resolves to `models` — outside the route
        // folder → not flagged (no false positive).
        let src = r"
            use super::super::models::Bar;
            pub fn handler(Json(b): Json<Bar>) -> String { String::new() }
        ";
        assert!(run(src, "handler", &[]).is_ok());
    }

    #[test]
    fn missing_ast_receiver_and_non_extractor_are_skipped() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route("handler", "missing.rs"));
        assert!(check_extractors(&metadata, &HashMap::new()).is_ok());

        let src = "struct Local; fn handler(&self, value: Local) {}";
        assert!(run(src, "handler", &[]).is_ok());
    }

    #[test]
    fn request_extractor_inner_rejects_non_path_bare_and_lifetime_arguments() {
        let reference: Type = syn::parse_quote!(&Json<Local>);
        assert!(request_extractor_inner(&reference).is_none());
        let bare: Type = syn::parse_quote!(Json);
        assert!(request_extractor_inner(&bare).is_none());
        let lifetime: Type = syn::parse_quote!(Json<'static>);
        assert!(request_extractor_inner(&lifetime).is_none());
    }

    #[test]
    fn custom_type_collection_recurse_reference_and_ignores_tuple() {
        let mut idents = Vec::new();
        collect_custom_type_idents(&syn::parse_quote!(&Vec<Local>), &mut idents);
        assert_eq!(idents, ["Vec", "Local"]);
        collect_custom_type_idents(&syn::parse_quote!((Local,)), &mut idents);
        assert_eq!(idents, ["Vec", "Local"]);
    }

    #[test]
    fn resolve_use_prefix_handles_self_and_rejects_invalid_prefixes() {
        let self_use: syn::ItemUse = syn::parse_quote!(
            use self::types::Local;
        );
        let (resolved_base, _) = resolve_use_prefix(&self_use.tree, &["routes", "handler"])
            .expect("self prefix resolves");
        assert_eq!(resolved_base, ["routes", "handler"]);

        let bare_import: syn::ItemUse = syn::parse_quote!(
            use Local;
        );
        assert!(resolve_use_prefix(&bare_import.tree, &["routes"]).is_none());
        let external: syn::ItemUse = syn::parse_quote!(
            use external::Local;
        );
        assert!(resolve_use_prefix(&external.tree, &["routes"]).is_none());
        let too_many_supers: syn::ItemUse = syn::parse_quote!(
            use super::super::Local;
        );
        assert!(resolve_use_prefix(&too_many_supers.tree, &["routes"]).is_none());
    }

    #[test]
    fn grouped_renamed_and_glob_route_imports_are_walked() {
        let ast: syn::File = syn::parse_quote! {
            use crate::routes::other::{Bar, Baz as B, *};
        };
        let sibling: syn::File = syn::parse_quote! {
            pub struct Bar;
            pub enum Baz { Value }
        };
        let route_module_files = HashMap::from([("routes::other", "other.rs")]);
        let file_cache = HashMap::from([("other.rs".to_string(), sibling)]);
        let mut out = HashSet::new();
        collect_imported_route_types(
            &ast,
            "routes::handler",
            &route_module_files,
            &file_cache,
            &HashSet::new(),
            &mut out,
        );
        assert_eq!(out, HashSet::from(["Bar".to_string(), "B".to_string()]));
    }

    #[test]
    fn file_declares_type_accepts_enums_and_ignores_other_items() {
        let ast: syn::File = syn::parse_quote! { enum Kind { A } fn helper() {} };
        assert!(file_declares_type(&ast, &syn::parse_quote!(Kind)));
        assert!(!file_declares_type(&ast, &syn::parse_quote!(helper)));
    }
}
