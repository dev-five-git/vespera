//! Compile-fail: two different `#[derive(Schema)]` items claim the OpenAPI name
//! `Dup`. The second derive reports the duplicate-name error, and it keeps its
//! other output (the `Schema` marker impl), so the use of `b::Dup` below adds
//! no "trait bound not satisfied" cascade next to that one diagnostic.

mod a {
    #[derive(vespera::Schema)]
    pub struct Dup {
        pub id: i32,
    }
}

mod b {
    #[derive(vespera::Schema)]
    pub struct Dup {
        pub name: String,
    }
}

fn requires_schema<T: vespera::Schema>() {}

fn main() {
    let _ = a::Dup { id: 1 };
    requires_schema::<b::Dup>();
}
