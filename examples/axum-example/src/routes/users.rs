use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{Json, extract::Path},
};

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct User {
    pub id: u32,
    pub name: String,
    pub email: String,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct CreateUserRequest {
    pub name: String,
    pub email: String,
}

/// Get all users
#[vespera::route(get)]
pub async fn get_users() -> Json<Vec<User>> {
    Json(vec![User { id: 1, name: "Alice".to_string(), email: "alice@example.com".to_string() }, User { id: 2, name: "Bob".to_string(), email: "bob@example.com".to_string() }])
}

/// Get user by ID
#[vespera::route(get, path = "/{id}")]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    Json(User { id, name: format!("User {}", id), email: format!("user{}@example.com", id) })
}

/// Create a new user
#[vespera::route(post)]
pub async fn create_user(Json(user): Json<CreateUserRequest>) -> Json<User> {
    Json(User { id: 100, name: user.name, email: user.email })
}
