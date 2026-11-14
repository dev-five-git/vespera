//! `crate::` paths in an integration test resolve from the test crate's root
//! (this file, modules under `tests/`): they used to be read from the
//! library's `src/lib.rs`, so `crate::schema_type_support::Shared` was "not
//! found".
#![deny(warnings)]

mod schema_type_support;

use schema_type_support::{DeepDto, Shared, nested::Deep};
use vespera::schema_type;

schema_type!(SharedName from crate::schema_type_support::Shared, pick = ["name"]);

#[test]
fn test_crate_paths_resolve() {
    let dto = SharedName::from(Shared {
        id: 1,
        name: "n".into(),
    });
    assert_eq!(dto.name, "n");
    assert_eq!(DeepDto::from(Deep { level: 3 }).level, 3);
}
