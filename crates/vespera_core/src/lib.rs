//! Vespera Core - `OpenAPI` structure definitions
//!
//! Provides structures conforming to the `OpenAPI` 3.1 specification.
//!
//! This is an internal building block of [`vespera`](https://docs.rs/vespera);
//! applications should depend on `vespera`, which re-exports these types.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "cron")]
#[cfg_attr(docsrs, doc(cfg(feature = "cron")))]
pub mod cron;
pub mod docs;
pub mod openapi;
pub mod route;
pub mod schema;

pub use openapi::*;
pub use route::*;
pub use schema::*;
