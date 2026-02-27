use serde::{Deserialize, Serialize};
use vespera::axum::Json;
use vespera::axum::extract::Form;
use vespera::axum::extract::Multipart;
use vespera::{Schema, route};

// ============== Request/Response DTOs ==============

#[derive(Deserialize, Schema)]
pub struct SubscribeRequest {
    pub name: String,
    pub email: String,
}

#[derive(Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResponse {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub is_subscribed: bool,
}

#[derive(Deserialize, Schema)]
pub struct ContactFormRequest {
    pub name: String,
    pub email: String,
    pub subject: Option<String>,
    pub message: String,
}

#[derive(Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ContactFormResponse {
    pub success: bool,
    pub ticket_id: String,
}

// ============== Handlers ==============

/// Subscribe to newsletter via form submission
#[route(post, tags = ["form"])]
pub async fn subscribe(Form(input): Form<SubscribeRequest>) -> Json<SubscribeResponse> {
    Json(SubscribeResponse {
        id: 1,
        name: input.name,
        email: input.email,
        is_subscribed: true,
    })
}

/// Submit a contact form
#[route(post, path = "/contact", tags = ["form"])]
pub async fn contact(Form(input): Form<ContactFormRequest>) -> Json<ContactFormResponse> {
    Json(ContactFormResponse {
        success: true,
        ticket_id: format!(
            "TICKET-{}-{}",
            input.name.len() + input.message.len(),
            input.email.len() + input.subject.as_deref().map_or(0, str::len)
        ),
    })
}

/// Upload a file via raw multipart form data
#[route(post, path = "/upload", tags = ["form"])]
pub async fn upload(mut multipart: Multipart) -> Json<ContactFormResponse> {
    while let Some(field) = multipart.next_field().await.unwrap() {
        let _name = field.name().unwrap_or("unknown").to_string();
        let _data = field.bytes().await.unwrap();
    }
    Json(ContactFormResponse {
        success: true,
        ticket_id: "UPLOAD-001".to_string(),
    })
}
