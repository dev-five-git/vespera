//! Malformed `#[route(headers = [...])]` objects: missing `name`, a repeated
//! field, and an unknown field.
use vespera_macro::route;

#[route(get, headers = [{ required = true }])]
pub async fn header_missing_name() {}

#[route(get, headers = [{ name = "X-A", name = "X-B" }])]
pub async fn header_duplicate_name() {}

#[route(get, headers = [{ name = "X-A", kind = "string" }])]
pub async fn header_unknown_field() {}

fn main() {}
