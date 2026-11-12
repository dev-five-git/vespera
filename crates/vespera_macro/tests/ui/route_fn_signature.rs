//! `#[route]` handlers must be `pub async fn` — a private or synchronous
//! handler is rejected at compile time with a fix-it hint, and the attribute
//! cannot be placed on a non-function item.
use vespera_macro::route;

#[route(get)]
async fn not_public() {}

#[route(get)]
pub fn not_async() {}

#[route(get)]
pub struct NotAFunction;

fn main() {}
