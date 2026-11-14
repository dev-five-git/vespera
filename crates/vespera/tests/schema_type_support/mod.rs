//! A module of the `schema_type_test_crate` integration test: `crate::` here
//! is the test crate, whose root is `tests/schema_type_test_crate.rs`.

#[derive(Clone)]
pub struct Shared {
    pub id: i32,
    pub name: String,
}

pub mod nested {
    #[derive(Clone)]
    pub struct Deep {
        pub level: u8,
    }
}

// Resolved from this module (`crate::schema_type_support`), not from the
// library's `src/`.
vespera::schema_type!(DeepDto from crate::schema_type_support::nested::Deep);
