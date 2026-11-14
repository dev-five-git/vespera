//! A `Path<T>` extractor that can never deserialize the route's own path
//! parameters is a compile error on the `Path<..>` type instead of a `500` /
//! `400` on every request. The app may be nested under a parameterised
//! prefix (`Router::nest("/{tenant}", app)`), so extracting MORE values than
//! the route declares stays accepted.
use std::collections::HashMap;

use vespera_macro::route;

pub struct Path<T>(pub T);

#[derive(serde::Deserialize)]
pub struct OrgUser {
    pub org: String,
    pub id: u32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrictId {
    pub id: u32,
}

#[route(get, path = "/a/{x}/{y}/{z}")]
pub async fn tuple_too_short(_p: Path<(u32, u32)>) {}

#[route(get, path = "/{a}/{b}")]
pub async fn scalar_for_two(_p: Path<u32>) {}

#[route(get, path = "/{id}/{extra}")]
pub async fn strict_struct_unknown_segment(_p: Path<StrictId>) {}

// Accepted: exact tuple, scalar, struct, a map over several segments, and
// extractors that also read parameters of a nest prefix (`/{tenant}`).
#[route(get, path = "/ok/{x}/{y}")]
pub async fn tuple_ok(_p: Path<(u32, String)>) {}

#[route(get, path = "/ok/{id}")]
pub async fn scalar_ok(_p: Path<u32>) {}

#[route(get, path = "/ok/{org}/{id}/{extra}")]
pub async fn struct_ok(_p: Path<OrgUser>) {}

#[route(get, path = "/ok/{a}/{b}")]
pub async fn map_ok(_p: Path<HashMap<String, String>>) {}

#[route(get, path = "/nested/{id}")]
pub async fn tuple_with_nest_prefix(_p: Path<(String, u32)>) {}

#[route(get, path = "/nested")]
pub async fn scalar_from_nest_prefix(_p: Path<String>) {}

#[route(get, path = "/nested/{id}")]
pub async fn struct_field_from_nest_prefix(_p: Path<OrgUser>) {}

fn main() {}
