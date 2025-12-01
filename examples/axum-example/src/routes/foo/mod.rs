use serde::{Deserialize, Serialize};
use std::sync::Arc;
use vespera::{
    Schema,
    axum::{Json, extract::State},
};

use crate::AppState;

#[derive(Serialize, Deserialize, Schema)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(rename_all = "camelCase")]
pub struct SignupResponse {
    pub id: i32,
    pub email: String,
    pub name: String,
    #[serde(rename = "phoneNumber23")]
    pub phone_number: String,
    pub nickname: Option<String>,
    pub birthday: Option<String>,
    pub gender: Option<String>,
    pub job: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[vespera::route(post, path = "/foo")]
pub async fn signup(
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<SignupRequest>,
) -> Result<Json<SignupResponse>, String> {
    println!("app_state: {:?}", app_state.config);
    let response = SignupResponse {
        id: 1,
        email: request.email,
        name: "John Doe".to_string(),
        phone_number: "1234567890".to_string(),
        nickname: Some("John".to_string()),
        birthday: Some("1990-01-01".to_string()),
        gender: Some("male".to_string()),
        job: Some("engineer".to_string()),
        created_at: "2021-01-01".to_string(),
    };
    Ok(Json(response))
}
