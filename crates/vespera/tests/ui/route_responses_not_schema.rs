//! Compile-fail: a `#[route(responses = [...])]` type must derive `Schema`.
//! Otherwise the generated OpenAPI would carry a `$ref` to a component that
//! does not exist; the error points at the offending type.

pub struct NotFound {
    pub message: String,
}

#[vespera::route(get, responses = [(404, NotFound)])]
pub async fn handler() -> &'static str {
    "ok"
}

fn main() {}
