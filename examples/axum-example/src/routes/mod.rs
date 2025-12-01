use std::collections::HashMap;

use serde::Deserialize;
use vespera::{
    Schema,
    axum::{Json, extract::Query},
};

pub mod error;
pub mod foo;
pub mod health;
pub mod path;
pub mod users;

/// Health check endpoint
#[vespera::route(get)]
pub async fn root_endpoint() -> &'static str {
    "root endpoint"
}

#[vespera::route(get, path = "/hello")]
pub async fn mod_file_endpoint() -> &'static str {
    "mod file endpoint"
}

#[vespera::route(get, path = "/map-query")]
pub async fn mod_file_with_map_query(
    Query(_query): Query<HashMap<String, String>>,
) -> &'static str {
    "mod file endpoint"
}

#[derive(Deserialize, Schema)]
pub struct StructQuery {
    pub name: String,
    pub age: u32,
}

#[vespera::route(get, path = "/struct-query")]
pub async fn mod_file_with_struct_query(Query(query): Query<StructQuery>) -> String {
    format!("name: {}, age: {}", query.name, query.age)
}

#[derive(Deserialize, Schema)]
pub struct StructBody {
    pub name: String,
    pub age: u32,
}

#[vespera::route(post, path = "/struct-body")]
pub async fn mod_file_with_struct_body(Json(body): Json<StructBody>) -> String {
    format!("name: {}, age: {}", body.name, body.age)
}

#[derive(Deserialize, Schema, Debug)]
pub struct StructBodyWithOptional {
    pub name: Option<String>,
    pub age: Option<u32>,
}

#[vespera::route(post, path = "/struct-body-with-optional")]
pub async fn mod_file_with_struct_body_with_optional(
    Json(body): Json<StructBodyWithOptional>,
) -> String {
    format!("name: {:?}, age: {:?}", body.name, body.age)
}

#[derive(Deserialize, Schema)]
pub struct ComplexStructBody {
    pub name: String,
    pub age: u32,
    pub nested_struct: StructBodyWithOptional,
    pub array: Vec<String>,
    pub map: HashMap<String, String>,
    pub nested_array: Vec<StructBodyWithOptional>,
    pub nested_map: HashMap<String, StructBodyWithOptional>,
    pub nested_struct_array: Vec<StructBodyWithOptional>,
    pub nested_struct_map: HashMap<String, StructBodyWithOptional>,
    pub nested_struct_array_map: Vec<HashMap<String, StructBodyWithOptional>>,
    pub nested_struct_map_array: HashMap<String, Vec<StructBodyWithOptional>>,
}

#[vespera::route(post, path = "/complex-struct-body")]
pub async fn mod_file_with_complex_struct_body(Json(body): Json<ComplexStructBody>) -> String {
    format!(
        "name: {}, age: {}, nested_struct: {:?}, array: {:?}, map: {:?}, nested_array: {:?}, nested_map: {:?}, nested_struct_array: {:?}, nested_struct_map: {:?}, nested_struct_array_map: {:?}, nested_struct_map_array: {:?}",
        body.name,
        body.age,
        body.nested_struct,
        body.array,
        body.map,
        body.nested_array,
        body.nested_map,
        body.nested_struct_array,
        body.nested_struct_map,
        body.nested_struct_array_map,
        body.nested_struct_map_array
    )
}

#[derive(Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ComplexStructBodyWithRename {
    pub name: String,
    pub age: u32,
    pub nested_struct: StructBodyWithOptional,
    pub array: Vec<String>,
    pub map: HashMap<String, String>,
    pub nested_array: Vec<StructBodyWithOptional>,
    pub nested_map: HashMap<String, StructBodyWithOptional>,
    pub nested_struct_array: Vec<StructBodyWithOptional>,
    pub nested_struct_map: HashMap<String, StructBodyWithOptional>,
    pub nested_struct_array_map: Vec<HashMap<String, StructBodyWithOptional>>,
    pub nested_struct_map_array: HashMap<String, Vec<StructBodyWithOptional>>,
}

#[vespera::route(post, path = "/complex-struct-body-with-rename")]
pub async fn mod_file_with_complex_struct_body_with_rename(
    Json(body): Json<ComplexStructBodyWithRename>,
) -> String {
    format!(
        "name: {}, age: {}, nested_struct: {:?}, array: {:?}, map: {:?}, nested_array: {:?}, nested_map: {:?}, nested_struct_array: {:?}, nested_struct_map: {:?}, nested_struct_array_map: {:?}, nested_struct_map_array: {:?}",
        body.name,
        body.age,
        body.nested_struct,
        body.array,
        body.map,
        body.nested_array,
        body.nested_map,
        body.nested_struct_array,
        body.nested_struct_map,
        body.nested_struct_array_map,
        body.nested_struct_map_array
    )
}
