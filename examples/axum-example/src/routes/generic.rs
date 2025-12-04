use serde::Serialize;
use vespera::axum::Json;

use crate::TestStruct;

#[derive(Serialize, vespera::Schema)]
pub struct GenericStruct<T: Serialize> {
    pub value: T,
    pub name: String,
}

#[derive(Serialize, vespera::Schema)]
pub struct GenericStruct2<T, T2> {
    pub value: T,
    pub name: String,
    pub value2: T2,
}

#[vespera::route(get, path = "/generic/{value}")]
pub async fn generic_endpoint(
    vespera::axum::extract::Path(value): vespera::axum::extract::Path<String>,
) -> Json<GenericStruct<String>> {
    Json(GenericStruct {
        value,
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

#[vespera::route(get, path = "/generic3")]
pub async fn generic_endpoint3() -> Json<GenericStruct2<TestStruct, String>> {
    Json(GenericStruct2 {
        value: TestStruct {
            name: "test".to_string(),
            age: 20,
        },
        value2: "test2".to_string(),
        name: "John Doe".to_string(),
    })
}

#[vespera::route(get, path = "/generic4")]
pub async fn generic_endpoint4() -> Json<GenericStruct2<bool, bool>> {
    Json(GenericStruct2 {
        value: true,
        value2: false,
        name: "John Doe".to_string(),
    })
}
