//! A projection of a type parameter qualified by its bound (`T::Item` with
//! `T: IntoIterator<Item = Entry>`) instantiated with a concrete type
//! (`Page<Vec<Entry>>`) must compile: the generated DTO names the projection as
//! `<Vec<Entry> as IntoIterator>::Item`. A binding inside that qualified path
//! (`<Vec<Entry> as IntoIterator<Item = Entry>>::Item`) is E0229.
#![deny(warnings)]

use serde::{Deserialize, Serialize};
use vespera::{Schema, schema, schema_type};

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct Entry {
    pub id: u32,
}

#[derive(Clone, Schema)]
pub struct Page<T: IntoIterator<Item = Entry>> {
    pub first: T::Item,
    pub rest: Vec<T::Item>,
}

schema_type!(FirstPage from Page<Vec<Entry>>);

#[test]
fn projection_through_a_bound_with_a_binding_instantiates() {
    let page = FirstPage {
        first: Entry { id: 1 },
        rest: vec![Entry { id: 2 }],
    };
    assert_eq!((page.first.id, page.rest.len()), (1, 1));
    let schema = schema!(Page<Vec<Entry>>);
    let properties = schema.properties.expect("properties");
    assert!(properties.contains_key("first"), "{properties:?}");
    assert!(properties.contains_key("rest"), "{properties:?}");
}
