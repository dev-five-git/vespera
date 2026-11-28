//! Which type paths name the JSON value types (`serde_json::Value`, SeaORM's
//! `Json` / `JsonValue`, `serde_json::Map`). Sidecar of `conversion`.

use std::{borrow::Borrow, collections::HashSet, hash::Hash};

/// `Map` through a `serde_json` segment anywhere before it (`serde_json::Map`,
/// `vespera::serde_json::Map`, `::serde_json::Map`, and the `serde_json::map::Map`
/// module path): the object type, whose members are the value type `V` of
/// `Map<String, V>`. A bare `Map` (the `use serde_json::Map;` spelling) is the
/// same object type unless it names a known component schema, as the bare
/// `Value` of [`is_json_value_path`] does. Here a bare `Map` is not checked for
/// its import: the derive's `Schema` assertion rejects an unimported `Map`
/// before the conversion sees it (`schema_assertions/map_leaf.rs`).
pub(super) fn is_serde_json_map_path(
    path: &syn::Path,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
) -> bool {
    if path.segments.last().is_none_or(|last| last.ident != "Map") {
        return false;
    }
    if path.segments.len() == 1 {
        return !known_schemas.contains("Map");
    }
    path.segments
        .iter()
        .rev()
        .skip(1)
        .any(|segment| segment.ident == "serde_json")
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
