//! Compiled out (`#[cfg(any())] pub mod disabled;`): the router mounts this
//! handler under the same `#[cfg]`, so it never appears.

#[vespera::route(get)]
pub async fn hidden() -> &'static str {
    "hidden"
}
