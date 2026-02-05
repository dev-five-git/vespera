use std::collections::HashMap;

use serde::Deserialize;
use vespera::{
    Schema,
    axum::{Json, extract::Query},
};

use crate::TestStruct;

pub mod enums;
pub mod error;
pub mod flatten;
pub mod foo;
pub mod generic;
pub mod health;
pub mod memos;
pub mod path;
pub mod typed_header;
pub mod users;

/// Health check endpoint
#[vespera::route(get)]
pub async fn root_endpoint() -> &'static str {
    "root endpoint"
}

/// Hello!!
#[vespera::route(get, path = "/hello", tags = ["hello"])]
pub async fn mod_file_endpoint() -> &'static str {
    "mod file endpoint"
}

#[derive(Deserialize, Schema, Debug)]
pub struct MapQuery {
    pub name: String,
    pub age: u32,
    pub optional_age: Option<u32>,
}
#[vespera::route(get, path = "/map-query")]
pub async fn mod_file_with_map_query(Query(query): Query<MapQuery>) -> &'static str {
    println!("map query: {:?}", query.age);
    println!("map query: {:?}", query.name);
    println!("map query: {:?}", query.optional_age);
    "mod file endpoint"
}

#[derive(Deserialize, Debug)]
pub struct NoSchemaQuery {
    pub name: String,
    pub age: u32,
    pub optional_age: Option<u32>,
}

#[vespera::route(get, path = "/no-schema-query")]
pub async fn mod_file_with_no_schema_query(Query(query): Query<NoSchemaQuery>) -> &'static str {
    println!("no schema query: {:?}", query.age);
    println!("no schema query: {:?}", query.name);
    println!("no schema query: {:?}", query.optional_age);
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

#[vespera::route(get, path = "/test_struct")]
pub async fn mod_file_with_test_struct(Query(query): Query<TestStruct>) -> Json<TestStruct> {
    Json(query)
}
