//! Compile-fail: a `#[route(responses = [...])]` type with generic arguments
//! is documented by its name alone, so `ErrorBody::<Detail>` would silently
//! become the bare `ErrorBody` in the spec — reject it instead.

#[vespera::route(get, responses = [(404, ErrorBody::<Detail>)])]
pub async fn handler() -> &'static str {
    "ok"
}

fn main() {}
