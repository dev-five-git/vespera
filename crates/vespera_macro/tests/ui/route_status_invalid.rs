//! `status` is the SUCCESS status and must be 2xx; `error_status` entries must
//! be integer literals in the u16 range.
use vespera_macro::route;

#[route(post, status = 404)]
pub async fn status_not_success() {}

#[route(get, error_status = ["404"])]
pub async fn error_status_not_integer() {}

#[route(get, error_status = [70000])]
pub async fn error_status_out_of_range() {}

fn main() {}
