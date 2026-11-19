//! A non-generic `#[derive(Schema)]` struct with a `PhantomData<T>` field
//! compiles (round-27 macrorest-4 M1): the assertion that every field type
//! implements `Schema` skips `PhantomData`, which has no `Schema` impl.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use vespera::Schema;

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Account {
    pub id: u64,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct UserId {
    pub raw: u64,
    #[serde(skip)]
    pub _marker: PhantomData<Account>,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Tagged {
    pub label: String,
    pub _marker: PhantomData<Account>,
}

#[test]
fn phantom_data_fields_compile_and_keep_the_other_fields() {
    let value = Tagged {
        label: "x".to_string(),
        _marker: PhantomData,
    };
    let json = serde_json::to_value(&value).unwrap();
    assert_eq!(json["label"], "x");
    let id = UserId {
        raw: 7,
        _marker: PhantomData,
    };
    assert_eq!(id.raw, 7);
}
