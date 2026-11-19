//! Which type paths name the JSON value types (`serde_json::Value`, SeaORM's
//! `Json` / `JsonValue`, `serde_json::Map`). Sidecar of `conversion`.

use std::{borrow::Borrow, collections::HashSet, hash::Hash};

/// `Map` through a `serde_json` segment (`serde_json::Map`,
/// `vespera::serde_json::Map`, `::serde_json::Map`): the object type.
pub(super) fn is_serde_json_map_path(path: &syn::Path) -> bool {
    path.segments.last().is_some_and(|last| last.ident == "Map")
        && path
            .segments
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|parent| parent.ident == "serde_json")
}

/// `serde_json::Value` under any of its names:
///
/// - `Value` through a `serde_json` segment (`serde_json::Value`,
///   `vespera::serde_json::Value`);
/// - SeaORM's aliases `Json` (`sea_orm::prelude::Json`) and `JsonValue`
///   (`sea_orm::JsonValue`), qualified or not — but only WITHOUT generic
///   arguments: `Json<T>` is axum's body wrapper, not a JSON value;
/// - a `Value` / `Json` / `JsonValue` that is not a known component schema
///   (a user `#[derive(Schema)] struct Json` keeps its `$ref`, also as
///   `crate::models::Json`) unless the path names `serde_json` / `sea_orm`.
pub(super) fn is_json_value_path(
    path: &syn::Path,
    last: &syn::PathSegment,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
) -> bool {
    let alias = last.ident == "Json" || last.ident == "JsonValue";
    if !(alias || last.ident == "Value") || !last.arguments.is_none() {
        return false;
    }
    let qualified = path
        .segments
        .iter()
        .any(|segment| segment.ident == "serde_json" || (alias && segment.ident == "sea_orm"));
    qualified
        || ((alias || path.segments.len() == 1)
            && !known_schemas.contains(last.ident.to_string().as_str()))
}
