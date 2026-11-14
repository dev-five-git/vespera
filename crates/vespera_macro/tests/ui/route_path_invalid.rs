//! `path` accepts axum 0.8 (`{id}`, `{*rest}`) and axum 0.7 (`:id`, `*rest`)
//! segments; a malformed template is a compile error on the path literal
//! instead of an axum panic when the router is built.
use vespera_macro::route;

#[route(get, path = "/users/{id")]
pub async fn unclosed_brace() {}

#[route(get, path = "/users/{a{b}}")]
pub async fn nested_brace() {}

#[route(get, path = "/users/{}")]
pub async fn empty_brace() {}

#[route(get, path = "/users/:")]
pub async fn empty_colon() {}

#[route(get, path = "/users/:id/posts/{id}")]
pub async fn duplicate_param() {}

#[route(get, path = "/files/*rest/meta")]
pub async fn catch_all_not_last() {}

// Accepted: mixed styles.
#[route(get, path = "/ok/:id/{post}/*rest")]
pub async fn mixed_styles_accepted() {}

fn main() {}
