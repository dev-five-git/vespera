//! Locate the function a `#[serde(default = "path")]` names, from the
//! deriving struct's own module.
//!
//! A bare name is looked up in the module that DEFINES the struct — the
//! inline `mod` it sits in, not the file's top level (where a same-named
//! function of another module would be picked, or none found).  A
//! `crate::a::b::f` path (what `schema_type!` re-roots a source field's
//! default function to) is looked up in that module's items; `self::` /
//! `super::` / `child::f` walk the inline modules of the struct's file.

use std::path::Path;

use crate::parser::strip_raw_prefix_owned;
use crate::schema_macro::{file_cache, module_scope};

fn ident_name(ident: &syn::Ident) -> String {
    strip_raw_prefix_owned(ident.to_string())
}

/// The literal value the default function `fn_path` returns, for the struct
/// named `struct_name` (its ident starting at `line`) in `file_path`.
pub(super) fn resolve_default_fn(
    fn_path: &str,
    struct_name: &str,
    line: usize,
    file_path: &Path,
) -> Option<serde_json::Value> {
    let path: syn::Path = syn::parse_str(fn_path).ok()?;
    if path.leading_colon.is_some() {
        return None;
    }
    let segments: Vec<String> = path.segments.iter().map(|s| ident_name(&s.ident)).collect();
    let (fn_name, module) = segments.split_last()?;
    let items = if module.first().is_some_and(|first| first == "crate") {
        module_scope::module_items(module)?
    } else {
        let file = file_cache::get_parsed_file(file_path)?;
        let mut chain = module_scope::item_module_chain(&file.items, struct_name, line);
        for segment in module {
            match segment.as_str() {
                "self" => {}
                "super" => {
                    chain.pop()?;
                }
                child => chain.push(child.to_string()),
            }
        }
        if chain.is_empty() {
            // The file's top level: the per-file cache already holds it.
            return super::cached_default_functions(file_path)?
                .get(fn_name)
                .cloned();
        }
        inline_module_items(file.items, &chain)?
    };
    items.iter().find_map(|item| match item {
        syn::Item::Fn(func) if ident_name(&func.sig.ident) == *fn_name => {
            crate::openapi_generator::extract_default_value_from_function(func)
        }
        _ => None,
    })
}

/// The items of the inline module `chain` below `items`.
fn inline_module_items(items: Vec<syn::Item>, chain: &[String]) -> Option<Vec<syn::Item>> {
    chain.iter().try_fold(items, |items, segment| {
        items.into_iter().find_map(|item| match item {
            syn::Item::Mod(module) if ident_name(&module.ident) == *segment => {
                module.content.map(|(_, inner)| inner)
            }
            _ => None,
        })
    })
}
