
use serde::Deserialize;
use vespera::{
    Schema,
    axum::extract::Query,
};


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
pub struct ThirdMapQuery {
    pub name: String,
    pub age: u32,
    pub optional_age: Option<u32>,
}
#[vespera::route(get, path = "/map-query")]
pub async fn mod_file_with_map_query(Query(query): Query<ThirdMapQuery>) -> &'static str {
    println!("map query: {:?}", query.age);
    println!("map query: {:?}", query.name);
    println!("map query: {:?}", query.optional_age);
    "mod file endpoint"
}