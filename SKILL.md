---
name: vespera-api-development
description: Guide for developing fully automated OpenAPI/Axum APIs using the Vespera framework.
---

# Vespera API Development

Vespera is a fully automated OpenAPI engine for Axum with zero-config route and schema discovery.
It allows you to write Axum APIs and automatically generate OpenAPI 3.1 specifications.

## Instructions

### 1. Project Setup
Add `vespera` to your dependencies:
```toml
[dependencies]
vespera = "0.1.0"
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

### 2. Main Application
Use the `vespera!` macro in `main.rs` to initialize the app. This macro automatically scans your routes.

```rust
use vespera::vespera;

#[tokio::main]
async fn main() {
    // Basic initialization
    let app = vespera!(
        openapi = "openapi.json", // Optional: Generate OpenAPI spec
        title = "My API",         // Optional: API Title
        version = "1.0.0",        // Optional: API Version
        docs_url = "/docs"        // Optional: Serve Swagger UI
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

### 3. Route Handlers
Define routes in modules (e.g., `src/routes/`). Use the `#[vespera::route]` attribute.

```rust
use axum::{Json, Path, State};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct User {
    pub id: u32,
    pub name: String,
}

// GET /users/{id}
#[vespera::route(path = "/{id}")] // Defaults to GET if not specified
pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    Json(User { id, name: "Alice".into() })
}

// POST /users
#[vespera::route(post)]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    Json(user)
}
```

### 4. Tags and Description

Add tags to group routes and descriptions for OpenAPI documentation.

**Tags:** Use the `tags` parameter to group routes.

```rust
#[vespera::route(get, tags = ["users"])]
pub async fn list_users() -> Json<Vec<User>> { ... }

#[vespera::route(post, tags = ["users", "admin"])]
pub async fn create_user(Json(user): Json<User>) -> Json<User> { ... }
```

**Description:** Two ways to add descriptions:

1. **Doc comments (recommended):** Automatically extracted from `///` comments.
```rust
/// Get all users from the database
#[vespera::route(get)]
pub async fn list_users() -> Json<Vec<User>> { ... }
```

2. **Explicit `description` parameter:** Takes priority over doc comments.
```rust
#[vespera::route(get, description = "Custom description")]
pub async fn list_users() -> Json<Vec<User>> { ... }
```

**Combined example:**
```rust
/// Get user by ID
#[vespera::route(get, path = "/{id}", tags = ["users"])]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> { ... }
```

### 5. Error Handling
Vespera supports `Result<T, E>` return types. It automatically documents both the success capability (200 OK) and the error responses in the OpenAPI spec.

```rust
use axum::http::StatusCode;

#[derive(Serialize, Deserialize, Schema)]
pub struct ErrorResponse {
    pub message: String,
}

// Result<Json<Success>, Json<Error>>
#[vespera::route(get, path = "/may-fail")]
pub async fn may_fail() -> Result<Json<User>, Json<ErrorResponse>> {
    Err(Json(ErrorResponse { message: "Something went wrong".to_string() }))
}

// Result<Json<Success>, (StatusCode, Json<Error>)>
#[vespera::route(get, path = "/not-found")]
pub async fn not_found_example() -> Result<Json<User>, (StatusCode, Json<ErrorResponse>)> {
    Err((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse { message: "User not found".to_string() })
    ))
}
```

### 4. File Structure
The file structure in `src/routes` maps to URL paths:
- `src/routes/users.rs` -> `/users`
- `src/routes/admin/stats.rs` -> `/admin/stats`

## Guidelines
- Always use `vespera!` macro instead of manually building `Router::new()`.
- Use `#[vespera::route]` for all handlers to ensure they are picked up by the OpenAPI generator.
- `dir` parameter in `vespera!` defaults to `"routes"`.
- Handlers and schemas are automatically imported.
