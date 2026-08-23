use serde::Deserialize;
use vespera::Schema;
use vespera::axum::{
    Json,
    extract::{Path, Query},
};

use crate::models::schemas::{ApiResponse, ErrorBody, Paginated, Profile, Role, User};

#[derive(Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ListUsersQuery {
    pub page: u32,
    pub per_page: u32,
    pub search: Option<String>,
}

/// List users (paginated).
#[vespera::route(get, tags = ["users"])]
pub async fn list_users(Query(_q): Query<ListUsersQuery>) -> Json<Paginated<User>> {
    Json(Paginated::empty())
}

/// Get one user.
#[vespera::route(get, path = "/{id}", tags = ["users"], responses = [(404, ErrorBody)])]
pub async fn get_user(Path(_id): Path<u64>) -> Json<ApiResponse<User>> {
    Json(ApiResponse::ok(User::default()))
}

/// Create a user.
#[vespera::route(post, tags = ["users"])]
pub async fn create_user(Json(body): Json<User>) -> Json<ApiResponse<User>> {
    Json(ApiResponse::ok(body))
}

/// Update a user.
#[vespera::route(put, path = "/{id}", tags = ["users"])]
pub async fn update_user(Path(_id): Path<u64>, Json(body): Json<User>) -> Json<ApiResponse<User>> {
    Json(ApiResponse::ok(body))
}

/// A user's roles.
#[vespera::route(get, path = "/{id}/roles", tags = ["users"])]
pub async fn user_roles(Path(_id): Path<u64>) -> Json<Vec<Role>> {
    Json(Vec::new())
}

/// A user's profile.
#[vespera::route(get, path = "/{id}/profile", tags = ["users"])]
pub async fn user_profile(Path(_id): Path<u64>) -> Json<Profile> {
    Json(Profile::default())
}
