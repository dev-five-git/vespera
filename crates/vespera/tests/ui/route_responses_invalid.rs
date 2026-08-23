//! Compile-fail: a malformed `#[route(responses = [...])]` entry must be a
//! clean, span-attached compile error — not silently dropped by the extraction
//! `filter_map` (which previously emitted incomplete OpenAPI with no warning).
//!
//! `(404)` is a parenthesized expression, not a `(status, Type)` tuple, so it
//! is missing the response type.

#[vespera::route(get, responses = [(404)])]
pub async fn handler() -> &'static str {
    "ok"
}

fn main() {}
