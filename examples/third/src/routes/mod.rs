use serde::Deserialize;
use vespera::{
    Schema,
    axum::extract::Query,
};

/// Third app root endpoint
#[vespera::route(get, path = "/third")]
pub async fn third_root_endpoint() -> &'static str {
    "third app root endpoint"
}

/// Third app hello endpoint
#[vespera::route(get, path = "/third/hello", tags = ["third"])]
pub async fn third_hello_endpoint() -> &'static str {
    "third app hello endpoint"
}

#[derive(Deserialize, Schema, Debug)]
pub struct ThirdMapQuery {
    pub name: String,
    pub age: u32,
    pub optional_age: Option<u32>,
}

#[vespera::route(get, path = "/third/map-query", tags = ["third"])]
pub async fn third_map_query(Query(query): Query<ThirdMapQuery>) -> &'static str {
    println!("third map query: {:?}", query.age);
    println!("third map query: {:?}", query.name);
    println!("third map query: {:?}", query.optional_age);
    "third app map query endpoint"
}