//! `status` is the SUCCESS status and must be 2xx; `error_status` entries must
//! be integer literals naming an HTTP status code (100-599).
use vespera_macro::route;

#[route(post, status = 404)]
pub async fn status_not_success() {}

#[route(get, error_status = ["404"])]
pub async fn error_status_not_integer() {}

#[route(get, error_status = [70000])]
pub async fn error_status_out_of_range() {}

#[route(get, error_status = [1000])]
pub async fn error_status_not_http() {}

fn main() {}
