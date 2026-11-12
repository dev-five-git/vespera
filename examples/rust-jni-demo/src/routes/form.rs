//! Form-urlencoded routes — the JNI E2E suite drives these through Spring,
//! where `FormContentFilter` (PUT/PATCH/DELETE) or an earlier
//! `getParameter()` call (POST) consumes the servlet body before the proxy
//! reads it; the bridge rebuilds the body so the `Form<T>` extractor here
//! still sees every field.

use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{Form, Json},
};

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct ProfileForm {
    pub name: String,
    pub count: u32,
    #[serde(default)]
    pub note: Option<String>,
}

/// Echo a form-urlencoded profile update as JSON.
#[allow(clippy::unused_async)]
#[vespera::route(patch, tags = ["form"])]
pub async fn update_profile(Form(form): Form<ProfileForm>) -> Json<ProfileForm> {
    Json(form)
}

/// Echo a form-urlencoded profile submission as JSON.
#[allow(clippy::unused_async)]
#[vespera::route(post, tags = ["form"])]
pub async fn create_profile(Form(form): Form<ProfileForm>) -> Json<ProfileForm> {
    Json(form)
}
