use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{Json, http::StatusCode, http::header::HeaderMap, response::IntoResponse},
};

#[derive(Serialize, Deserialize, Schema)]
pub struct ErrorResponse {
    pub error: String,
    pub code: u32,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ErrorResponse2 {
    pub error: String,
    pub code: u32,
}

impl IntoResponse for ErrorResponse2 {
    fn into_response(self) -> vespera::axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(self)).into_response()
    }
}

#[vespera::route()]
pub async fn error_endpoint() -> Result<&'static str, Json<ErrorResponse>> {
    Err(Json(ErrorResponse {
        error: "Internal server error".to_string(),
        code: 500,
    }))
}

#[vespera::route(path = "/error-with-status")]
pub async fn error_endpoint_with_status_code()
-> Result<&'static str, (StatusCode, Json<ErrorResponse>)> {
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "Internal server error".to_string(),
            code: 500,
        }),
    ))
}

#[vespera::route(path = "/error2")]
pub async fn error_endpoint2() -> Result<&'static str, ErrorResponse2> {
    Err(ErrorResponse2 {
        error: "Internal server error".to_string(),
        code: 500,
    })
}

#[vespera::route(path = "/error-with-status2", error_status = [500, 400, 404])]
pub async fn error_endpoint_with_status_code2() -> Result<&'static str, (StatusCode, ErrorResponse2)>
{
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorResponse2 {
            error: "Internal server error".to_string(),
            code: 500,
        },
    ))
}

#[vespera::route(path = "/header-map")]
pub async fn header_map_endpoint() -> Result<(HeaderMap, &'static str), ErrorResponse2> {
    let headers = HeaderMap::new();
    println!("headers: {:?}", headers);
    Ok((headers, "ok"))
}

#[vespera::route(path = "/header-map2")]
pub async fn header_map_endpoint2() -> Result<(StatusCode, HeaderMap, &'static str), ErrorResponse2>
{
    let headers = HeaderMap::new();
    println!("headers: {:?}", headers);
    Ok((StatusCode::INTERNAL_SERVER_ERROR, headers, "ok"))
}
