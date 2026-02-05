use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use vespera::{Schema, axum::Json};

use crate::TestStruct;

#[derive(Serialize, Deserialize, Schema)]
pub enum Enum {
    A,
    B,
    C,
}

#[vespera::route(get)]
pub async fn enum_endpoint() -> Json<Enum> {
    Json(Enum::A)
}

/// Enum2 Description
#[derive(Serialize, Deserialize, Schema)]
pub enum Enum2 {
    A(String),
    B { name: String, age: i32 },
    C(i32),
    D(bool),
    E(Vec<String>),
    F(String, i32),
    G(HashMap<String, String>),
    H(BTreeMap<String, String>),
    I(TestStruct),
    J(Vec<TestStruct>),
    K(TestStruct, TestStruct),
    L(Option<String>),
    M(Vec<Option<String>>),
    N(HashMap<String, Option<String>>),
}

#[vespera::route(get, path = "/enum2")]
pub async fn enum_endpoint2() -> Json<Enum2> {
    Json(Enum2::A("a".to_string()))
}

// === Serde Enum Representation Examples ===

/// Internally tagged enum - serializes as `{"type": "...", ...fields...}`
/// Example: `{"type": "Request", "id": 1, "method": "GET"}`
#[derive(Serialize, Deserialize, Schema)]
#[serde(tag = "type")]
pub enum InternallyTaggedMessage {
    /// A request message
    Request { id: i32, method: String },
    /// A response message
    Response { id: i32, result: Option<String> },
    /// A notification (no payload)
    Notification,
}

#[vespera::route(post, path = "/internally-tagged")]
pub async fn internally_tagged_endpoint(
    Json(msg): Json<InternallyTaggedMessage>,
) -> Json<InternallyTaggedMessage> {
    Json(msg)
}

/// Adjacently tagged enum - serializes as `{"type": "...", "data": ...}`
/// Example: `{"type": "Success", "data": {"items": ["a", "b"]}}`
#[derive(Serialize, Deserialize, Schema)]
#[serde(tag = "type", content = "data")]
pub enum AdjacentlyTaggedResponse {
    /// Successful response with items
    Success { items: Vec<String> },
    /// Error response with code and message
    Error { code: i32, message: String },
    /// Empty response (unit variant)
    Empty,
}

#[vespera::route(post, path = "/adjacently-tagged")]
pub async fn adjacently_tagged_endpoint(
    Json(resp): Json<AdjacentlyTaggedResponse>,
) -> Json<AdjacentlyTaggedResponse> {
    Json(resp)
}

/// Untagged enum - serializes as just the variant data, no tag
/// The deserializer tries each variant in order until one matches.
/// Example: `"hello"` or `42` or `{"key": "value"}`
#[derive(Serialize, Deserialize, Schema)]
#[serde(untagged)]
pub enum UntaggedValue {
    /// A string value
    Text(String),
    /// A numeric value
    Number(i64),
    /// A boolean value
    Bool(bool),
    /// An object with key-value pairs
    Object { key: String, value: String },
}

#[vespera::route(post, path = "/untagged")]
pub async fn untagged_endpoint(Json(value): Json<UntaggedValue>) -> Json<UntaggedValue> {
    Json(value)
}

/// Externally tagged enum (default) - serializes as `{"VariantName": ...}`
/// Example: `{"Created": {"id": 1, "name": "test"}}`
/// This is included for comparison with the other representations.
#[derive(Serialize, Deserialize, Schema)]
pub enum ExternallyTaggedEvent {
    /// Item was created
    Created { id: i32, name: String },
    /// Item was updated
    Updated { id: i32 },
    /// Item was deleted
    Deleted(i32),
}

#[vespera::route(post, path = "/externally-tagged")]
pub async fn externally_tagged_endpoint(
    Json(event): Json<ExternallyTaggedEvent>,
) -> Json<ExternallyTaggedEvent> {
    Json(event)
}
