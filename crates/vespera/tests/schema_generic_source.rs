//! Generic sources take the macro's type arguments: `schema!(Page<Item>)`
//! used to document an unresolved `T`, and `schema_type!(Dto from
//! Page<Item>)` emitted fields of a type `T` the call site did not have.
//! `schema!` filters also reach into `#[serde(flatten)]` structs (they used
//! to filter nothing) and keep a struct `#[serde(tag)]`.
#![deny(warnings)]

use serde::{Deserialize, Serialize};
use vespera::schema::{SchemaRef, SchemaType};
use vespera::{Schema, schema, schema_type};

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Item {
    pub id: u32,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Page<T, M = ()> {
    pub items: Vec<T>,
    pub total: u64,
    pub meta: M,
}

schema_type!(ItemPage from Page<Item>, omit = ["meta"], add = [("cursor": Option<String>)]);
schema_type!(CountedPage from Page<Item, u32>);

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Audit {
    pub created: i64,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(tag = "kind")]
pub struct Note {
    pub id: u32,
    pub text: String,
    #[serde(flatten)]
    pub audit: Audit,
}

#[test]
fn schema_type_instantiates_generic_sources() {
    let page = ItemPage {
        items: vec![Item { id: 1 }],
        total: 1,
        cursor: None,
    };
    assert_eq!(page.items[0].id, 1);
    let counted = CountedPage::from(Page {
        items: vec![Item { id: 2 }],
        total: 1,
        meta: 7_u32,
    });
    assert_eq!((counted.items[0].id, counted.meta), (2, 7));
}

#[test]
fn schema_macro_instantiates_generic_sources() {
    let schema = schema!(Page<Item>, pick = ["items"]);
    let properties = schema.properties.expect("properties");
    let SchemaRef::Inline(items) = &properties["items"] else {
        panic!("items is an inline array");
    };
    assert_eq!(items.schema_type, Some(SchemaType::Array));
    let Some(SchemaRef::Ref(item)) = items.items.as_ref() else {
        panic!("items reference `Item`: {items:?}");
    };
    assert_eq!(item.ref_path, "#/components/schemas/Item");
}

#[test]
fn schema_macro_filters_flattened_structs() {
    let picked = schema!(Note, pick = ["text"]);
    assert!(picked.all_of.is_none());
    let keys: Vec<_> = picked.properties.expect("properties").into_keys().collect();
    assert_eq!(keys, ["kind", "text"]);

    let omitted = schema!(Note, omit = ["text"]);
    assert_eq!(omitted.all_of.expect("still flattened").len(), 2);
}
