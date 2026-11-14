#![allow(dead_code)]
//! A generic `schema_type!` source: callers in other modules pass their own
//! row types (`routes::generic::PageRow`), which this module cannot name.

/// A position in a paged listing (resolved in this module).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, vespera::Schema)]
pub struct Cursor {
    pub next: Option<String>,
}

/// A page of `T`; `M` defaults to a sample of the same rows.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Page<T, M = Vec<T>> {
    pub items: Vec<T>,
    pub cursor: Cursor,
    pub sample: M,
}
