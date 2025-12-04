use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use vespera::axum::{Json, extract::Query};

use crate::TestStruct;

#[derive(Serialize, Deserialize, vespera::Schema)]
pub struct GenericStruct<T: Serialize> {
    pub value: T,
    pub name: String,
}

#[vespera::route(get, path = "/generic/{value}")]
pub async fn generic_endpoint(
    Query(value): Query<GenericStruct<String>>,
) -> Json<GenericStruct<String>> {
    Json(GenericStruct {
        value: value.value,
        name: "John Doe".to_string(),
    })
}

#[vespera::route(get, path = "/generic2")]
pub async fn generic_endpoint2() -> Json<GenericStruct<TestStruct>> {
    Json(GenericStruct {
        value: TestStruct {
            name: "test".to_string(),
            age: 20,
        },
        name: "John Doe".to_string(),
    })
}
