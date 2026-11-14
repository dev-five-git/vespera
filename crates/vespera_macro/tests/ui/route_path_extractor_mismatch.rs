//! A `Path<T>` extractor that cannot deserialize the route's path parameters
//! is a compile error on the `Path<..>` type instead of a `500` / `400` on
//! every request.
use std::collections::HashMap;

use vespera_macro::route;

pub struct Path<T>(pub T);

#[derive(serde::Deserialize)]
pub struct OrgUser {
    pub org: String,
    pub id: u32,
    pub page: Option<u32>,
}

#[route(get, path = "/a/{x}/{y}/{z}")]
pub async fn tuple_too_short(_p: Path<(u32, u32)>) {}

#[route(get, path = "/{x}")]
pub async fn tuple_too_long(_p: Path<(u32, String, bool)>) {}

#[route(get, path = "/{a}/{b}")]
pub async fn scalar_for_two(_p: Path<u32>) {}

#[route(get, path = "/orgs/{org}")]
pub async fn struct_field_without_segment(_p: Path<OrgUser>) {}

// Accepted: exact tuple, scalar, struct (extra segment ignored by serde),
// and a map over several segments.
#[route(get, path = "/ok/{x}/{y}")]
pub async fn tuple_ok(_p: Path<(u32, String)>) {}

#[route(get, path = "/ok/{id}")]
pub async fn scalar_ok(_p: Path<u32>) {}

#[route(get, path = "/ok/{org}/{id}/{extra}")]
pub async fn struct_ok(_p: Path<OrgUser>) {}

#[route(get, path = "/ok/{a}/{b}")]
pub async fn map_ok(_p: Path<HashMap<String, String>>) {}

fn main() {}
