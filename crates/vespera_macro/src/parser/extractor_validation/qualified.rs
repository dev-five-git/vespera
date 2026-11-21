//! Qualified route-type paths: `Json<crate::routes::other::Bar>` and
//! `Json<super::other::Bar>` name a type of another route file without a `use`
//! import, so the import walk of the parent never sees them. Each multi-segment
//! path is resolved to its module the way a `use` tree is (`crate`, `self`,
//! `super` chains), then checked like an imported type. Bare and `self::`
//! names in the route's own module stay with the local-type check.

use std::collections::{HashMap, HashSet};

use syn::Type;

use super::{AstLookup, record_route_type};

/// Collect into `out` the ident of every qualified path in `ty` (through the
/// same shapes as `collect_custom_type_idents`) that names a non-`Schema`
/// struct or enum declared in another route file. `current_module` is the
/// route's own module path.
pub(super) fn collect_qualified_route_types(
    ty: &Type,
    current_module: &str,
    route_module_files: &HashMap<&str, &str>,
    file_cache: &dyn AstLookup,
    out: &mut Vec<String>,
) {
    match ty {
        Type::Path(type_path) => {
            if type_path.qself.is_none()
                && let Some(ident) = qualified_route_type(
                    &type_path.path,
                    current_module,
                    route_module_files,
                    file_cache,
                )
            {
                out.push(ident);
            }
            if let Some(segment) = type_path.path.segments.last()
                && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            {
                for arg in &args.args {
                    if let syn::GenericArgument::Type(inner) = arg {
                        collect_qualified_route_types(
                            inner,
                            current_module,
                            route_module_files,
                            file_cache,
                            out,
                        );
                    }
                }
            }
        }
        Type::Reference(syn::TypeReference { elem, .. })
        | Type::Array(syn::TypeArray { elem, .. })
        | Type::Slice(syn::TypeSlice { elem, .. })
        | Type::Paren(syn::TypeParen { elem, .. })
        | Type::Group(syn::TypeGroup { elem, .. }) => {
            collect_qualified_route_types(
                elem,
                current_module,
                route_module_files,
                file_cache,
                out,
            );
        }
        Type::Tuple(tuple) => {
            for elem in &tuple.elems {
                collect_qualified_route_types(
                    elem,
                    current_module,
                    route_module_files,
                    file_cache,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// The ident `path` names when it is a qualified path (at least two segments,
/// `crate`/`self`/`super` first) to a non-`Schema` struct or enum declared in a
/// route file.
fn qualified_route_type(
    path: &syn::Path,
    current_module: &str,
    route_module_files: &HashMap<&str, &str>,
    file_cache: &dyn AstLookup,
) -> Option<String> {
    if path.leading_colon.is_some() {
        return None;
    }
    let idents: Vec<&syn::Ident> = path.segments.iter().map(|segment| &segment.ident).collect();
    let (last, prefix) = idents.split_last()?;
    let module = module_of(prefix, current_module)?;
    let mut found = HashSet::new();
    record_route_type(
        &module,
        last,
        last,
        route_module_files,
        file_cache,
        &mut found,
    );
    found.into_iter().next()
}

/// The module path (`routes::other`) the leading `crate` / `self` / `super`
/// segments of a path resolve to, from `current_module`. `None` for any other
/// first segment (an external crate, a bare module) or a `super` above the
/// crate root.
fn module_of(prefix: &[&syn::Ident], current_module: &str) -> Option<Vec<String>> {
    let (head, rest) = prefix.split_first()?;
    let mut module: Vec<String> = match head.to_string().as_str() {
        "crate" => Vec::new(),
        "self" => current_module.split("::").map(str::to_owned).collect(),
        "super" => {
            let mut module: Vec<String> = current_module.split("::").map(str::to_owned).collect();
            module.pop()?;
            module
        }
        _ => return None,
    };
    for segment in rest {
        match segment.to_string().as_str() {
            "super" => {
                module.pop()?;
            }
            name => module.push(name.to_owned()),
        }
    }
    Some(module)
}
