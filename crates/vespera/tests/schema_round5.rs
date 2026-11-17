//! `#[derive(Schema)]` codegen that must compile: the generated
//! "every field type implements `Schema`" assertions skip fields serde never
//! touches (`#[serde(skip)]`) and field types that cannot be named outside
//! the item (`Self`, lifetimes, const generics), while a one-sided skip
//! still documents — and asserts — the field.
#![deny(warnings)]

use serde::{Deserialize, Serialize};
use vespera::Schema;

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Item {
    pub id: u32,
}

/// Not a `Schema` type: only ever behind `#[serde(skip)]`.
#[derive(Default, Clone)]
pub struct Cache {
    pub hits: u64,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct WithSkipped {
    pub id: u32,
    #[serde(skip)]
    pub cache: Cache,
    #[serde(skip_serializing)]
    pub item: Item,
}

#[derive(Serialize, Clone, Schema)]
pub struct Borrowed<'a> {
    pub name: &'a str,
    pub item: Item,
}

#[derive(Serialize, Clone, Schema)]
pub struct Counter<const N: usize> {
    pub count: u32,
}

#[derive(Serialize, Clone, Schema)]
pub struct Fixed<const N: usize> {
    pub counter: Counter<N>,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Tree {
    pub value: u32,
    pub children: Vec<Self>,
}

#[test]
fn derived_schemas_compile_and_document_their_fields() {
    let skipped = WithSkipped {
        id: 1,
        cache: Cache::default(),
        item: Item { id: 2 },
    };
    assert_eq!((skipped.id, skipped.cache.hits, skipped.item.id), (1, 0, 2));
    let borrowed = Borrowed {
        name: "n",
        item: Item { id: 3 },
    };
    assert_eq!((borrowed.name, borrowed.item.id), ("n", 3));
    let fixed = Fixed::<2> {
        counter: Counter { count: 4 },
    };
    assert_eq!(fixed.counter.count, 4);
    let tree = Tree {
        value: 6,
        children: Vec::new(),
    };
    assert_eq!((tree.value, tree.children.len()), (6, 0));

    // `schema!` is the response view: serde never writes the
    // `skip_serializing` `item` (the request view documents it).
    let schema = vespera::schema!(WithSkipped);
    let properties = schema.properties.expect("properties");
    let keys: Vec<&str> = properties.keys().map(String::as_str).collect();
    assert_eq!(keys, ["id"]);
}
