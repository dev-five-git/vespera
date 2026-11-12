//! Compile-fail: without the `multipart` feature (default-on, dropped by
//! `default-features = false`) `#[derive(vespera::Multipart)]` — which
//! `schema_type!(..., multipart)` also emits — names the missing feature
//! instead of failing on unresolved `vespera::multipart::...` paths.
//!
//! Runs only when `multipart` is off.

#[derive(vespera::Multipart)]
pub struct Upload {
    #[form_data(limit = "1MiB")]
    pub name: String,
}

fn main() {}
