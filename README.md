# Vespera

**FastAPI-like developer experience for Rust.** Zero-config OpenAPI 3.1 generation for Axum.

[![Crates.io](https://img.shields.io/crates/v/vespera.svg)](https://crates.io/crates/vespera)
[![Documentation](https://docs.rs/vespera/badge.svg)](https://docs.rs/vespera)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/dev-five-git/vespera/CI.yml?branch=main&label=CI)](https://github.com/dev-five-git/vespera/actions)
[![Codecov](https://img.shields.io/codecov/c/github/dev-five-git/vespera)](https://codecov.io/gh/dev-five-git/vespera)

```rust
// That's it. Swagger UI at /docs, OpenAPI at openapi.json
let app = vespera!(openapi = "openapi.json", docs_url = "/docs");
```

## Why Vespera?

| Feature | Vespera | Manual Approach |
|---------|---------|-----------------|
| Route registration | Automatic (file-based) | Manual `Router::new().route(...)` |
| OpenAPI spec | Generated at compile time | Hand-written or runtime generation |
| Schema extraction | From Rust types | Manual JSON Schema |
| Swagger UI | Built-in | Separate setup |
| Type safety | Compile-time verified | Runtime errors |

## Quick Start

### 1. Add Dependencies

```toml
[dependencies]
vespera = "0.1"
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

### 2. Create Route Handler

```
src/
├── main.rs
└── routes/
    └── users.rs
```

**`src/routes/users.rs`**:
```rust
use axum::{Json, Path};
use serde::{Deserialize, Serialize};
use vespera::Schema;

#[derive(Serialize, Deserialize, Schema)]
pub struct User {
    pub id: u32,
    pub name: String,
}

/// Get user by ID
#[vespera::route(get, path = "/{id}", tags = ["users"])]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    Json(User { id, name: "Alice".into() })
}

/// Create a new user
#[vespera::route(post, tags = ["users"])]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    Json(user)
}
```

### 3. Setup Main

**`src/main.rs`**:
```rust
use vespera::vespera;

#[tokio::main]
async fn main() {
    let app = vespera!(
        openapi = "openapi.json",
        title = "My API",
        docs_url = "/docs"
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Swagger UI: http://localhost:3000/docs");
    axum::serve(listener, app).await.unwrap();
}
```

### 4. Run

```bash
cargo run
# Open http://localhost:3000/docs
```

---

## Core Concepts

### File-Based Routing

File structure maps to URL paths automatically:

```
src/routes/
├── mod.rs           → /
├── users.rs         → /users
├── posts.rs         → /posts
└── admin/
    ├── mod.rs       → /admin
    └── stats.rs     → /admin/stats
```

### Route Handlers

Handlers must be `pub async fn` with the `#[vespera::route]` attribute:

```rust
// GET /users (default method)
#[vespera::route]
pub async fn list_users() -> Json<Vec<User>> { ... }

// POST /users
#[vespera::route(post)]
pub async fn create_user(Json(user): Json<User>) -> Json<User> { ... }

// GET /users/{id}
#[vespera::route(get, path = "/{id}")]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> { ... }

// Full options
#[vespera::route(put, path = "/{id}", tags = ["users"], description = "Update user")]
pub async fn update_user(...) -> ... { ... }
```

### Schema Derivation

Derive `Schema` on types used in request/response bodies:

```rust
#[derive(Serialize, Deserialize, vespera::Schema)]
#[serde(rename_all = "camelCase")]  // Serde attributes are respected
pub struct CreateUserRequest {
    pub user_name: String,          // → "userName" in OpenAPI
    pub email: String,
    #[serde(default)]
    pub bio: Option<String>,        // Optional field
}
```

### Supported Extractors

| Extractor | OpenAPI Mapping |
|-----------|-----------------|
| `Path<T>` | Path parameters |
| `Query<T>` | Query parameters |
| `Json<T>` | Request body (application/json) |
| `Form<T>` | Request body (form-urlencoded) |
| `TypedHeader<T>` | Header parameters |
| `State<T>` | Ignored (internal) |

### Error Handling

```rust
#[derive(Serialize, Schema)]
pub struct ApiError {
    pub message: String,
}

#[vespera::route(get, path = "/{id}")]
pub async fn get_user(Path(id): Path<u32>) -> Result<Json<User>, (StatusCode, Json<ApiError>)> {
    if id == 0 {
        return Err((StatusCode::NOT_FOUND, Json(ApiError { message: "Not found".into() })));
    }
    Ok(Json(User { id, name: "Alice".into() }))
}
```

---

## `vespera!` Macro Reference

```rust
let app = vespera!(
    dir = "routes",                    // Route folder (default: "routes")
    openapi = "openapi.json",          // Output path (writes file at compile time)
    title = "My API",                  // OpenAPI info.title
    version = "1.0.0",                 // OpenAPI info.version (default: CARGO_PKG_VERSION)
    docs_url = "/docs",                // Swagger UI endpoint
    redoc_url = "/redoc",              // ReDoc endpoint
    servers = [                        // OpenAPI servers
        { url = "https://api.example.com", description = "Production" },
        { url = "http://localhost:3000", description = "Development" }
    ],
    merge = [crate1::App1, crate2::App2]  // Merge child vespera apps
);
```

## `export_app!` Macro Reference

Export a vespera app for merging into other apps:

```rust
// Basic usage (scans "routes" folder by default)
vespera::export_app!(MyApp);

// Custom directory
vespera::export_app!(MyApp, dir = "api");
```

Generates a struct with:
- `MyApp::OPENAPI_SPEC: &'static str` - The OpenAPI JSON spec
- `MyApp::router() -> Router` - Function returning the Axum router

### Environment Variable Fallbacks

All parameters support environment variable fallbacks:

| Parameter | Environment Variable |
|-----------|---------------------|
| `dir` | `VESPERA_DIR` |
| `openapi` | `VESPERA_OPENAPI` |
| `title` | `VESPERA_TITLE` |
| `version` | `VESPERA_VERSION` |
| `docs_url` | `VESPERA_DOCS_URL` |
| `redoc_url` | `VESPERA_REDOC_URL` |
| `servers` | `VESPERA_SERVER_URL` + `VESPERA_SERVER_DESCRIPTION` |

**Priority**: Macro parameter > Environment variable > Default

---

## `schema_type!` Macro

Generate request/response types from existing structs. Perfect for creating API types from database models.

### Basic Usage

```rust
use vespera::schema_type;

// Pick specific fields only
schema_type!(CreateUserRequest from crate::models::user::Model, pick = ["name", "email"]);

// Omit specific fields  
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);

// Add new fields
schema_type!(UpdateUserRequest from crate::models::user::Model, pick = ["name"], add = [("id": i32)]);
```

### Cross-File References

Reference structs from other files using module paths:

```rust
// In src/routes/users.rs - references src/models/user.rs
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);
```

### Auto-Generated `From` Impl

When `add` is NOT used, a `From` impl is automatically generated:

```rust
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);

// Now you can do:
let model: Model = db.find_user(id).await?;
Json(model.into())  // Automatic conversion!
```

### Parameters

| Parameter | Description |
|-----------|-------------|
| `pick` | Include only specified fields |
| `omit` | Exclude specified fields |
| `rename` | Rename fields: `rename = [("old", "new")]` |
| `add` | Add new fields (disables auto `From` impl) |
| `clone` | Control Clone derive (default: true) |

---

## `schema!` Macro

Get a `Schema` value at runtime with optional field filtering. Useful for programmatic schema access.

```rust
use vespera::{Schema, schema};

#[derive(Schema)]
pub struct User {
    pub id: i32,
    pub name: String,
    pub password: String,
}

// Full schema
let full: vespera::schema::Schema = schema!(User);

// With fields omitted
let safe: vespera::schema::Schema = schema!(User, omit = ["password"]);

// With only specified fields
let summary: vespera::schema::Schema = schema!(User, pick = ["id", "name"]);
```

> **Note:** For creating request/response types, use `schema_type!` instead - it generates actual struct types with `From` impl.

---

## Advanced Usage

### Adding State

```rust
let app = vespera!(docs_url = "/docs")
    .with_state(AppState { db: pool });
```

### Adding Middleware

```rust
let app = vespera!(docs_url = "/docs")
    .layer(CorsLayer::permissive())
    .layer(TraceLayer::new_for_http());
```

### Multiple OpenAPI Files

```rust
let app = vespera!(
    openapi = ["openapi.json", "docs/api-spec.json"]
);
```

### Custom Route Folder

```rust
// Scans src/api/ instead of src/routes/
let app = vespera!("api");

// Or explicitly
let app = vespera!(dir = "api");
```

### Merging Multiple Vespera Apps

Combine routes and OpenAPI specs from multiple vespera apps at compile time:

**Child app (e.g., `third` crate):**
```rust
// src/lib.rs
mod routes;

// Export app for merging (dir defaults to "routes")
vespera::export_app!(ThirdApp);

// Or with custom directory
// vespera::export_app!(ThirdApp, dir = "api");
```

**Parent app:**
```rust
// src/main.rs
use vespera::vespera;

let app = vespera!(
    openapi = "openapi.json",
    docs_url = "/docs",
    merge = [third::ThirdApp]  // Merges router AND OpenAPI spec
)
.with_state(app_state);
```

This automatically:
- Merges all routes from child apps into the parent router
- Combines OpenAPI specs (paths, schemas, tags) into a single spec
- Makes Swagger UI show all routes from all apps

---

## Type Mapping

| Rust Type | OpenAPI Schema |
|-----------|----------------|
| `String`, `&str` | `string` |
| `i32`, `u64`, etc. | `integer` |
| `f32`, `f64` | `number` |
| `bool` | `boolean` |
| `Vec<T>` | `array` with items |
| `Option<T>` | nullable T |
| `HashMap<K, V>` | `object` with additionalProperties |
| Custom struct | `$ref` to components/schemas |

---

## Project Structure

```
vespera/
├── crates/
│   ├── vespera/           # Main crate - re-exports everything
│   ├── vespera_core/      # OpenAPI types and abstractions
│   └── vespera_macro/     # Proc-macros (compile-time magic)
└── examples/
    └── axum-example/      # Complete example application
```

---

## Contributing

```bash
git clone https://github.com/dev-five-git/vespera.git
cd vespera

# Build & test
cargo build
cargo test --workspace

# Run example
cd examples/axum-example
cargo run
# → http://localhost:3000/docs
```

See [SKILL.md](./SKILL.md) for development guidelines and architecture details.

---

## Comparison

### vs. utoipa

- **Vespera**: Zero-config, file-based routing, compile-time generation
- **utoipa**: Manual annotations, more control, works with any router

### vs. aide

- **Vespera**: Automatic discovery, built-in Swagger UI
- **aide**: More flexible, supports multiple doc formats

### vs. paperclip

- **Vespera**: Axum-first, modern OpenAPI 3.1
- **paperclip**: Actix-focused, OpenAPI 2.0/3.0

---

## License

Apache-2.0

---

## Acknowledgments

Inspired by [FastAPI](https://fastapi.tiangolo.com/)'s developer experience and [Next.js](https://nextjs.org/)'s file-based routing.
