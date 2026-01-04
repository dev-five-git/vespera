# Vespera
A fully automated OpenAPI engine for Axum with zero-config route and schema discovery.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![GitHub Actions](https://img.shields.io/github/actions/workflow/status/dev-five-git/vespera/CI.yml?branch=main&label=CI)](https://github.com/dev-five-git/vespera/actions)
[![Codecov](https://img.shields.io/codecov/c/github/dev-five-git/vespera)](https://codecov.io/gh/dev-five-git/vespera)
[![GitHub stars](https://img.shields.io/github/stars/dev-five-git/vespera.svg?style=social&label=Star)](https://github.com/dev-five-git/vespera)
[![GitHub forks](https://img.shields.io/github/forks/dev-five-git/vespera.svg?style=social&label=Fork)](https://github.com/dev-five-git/vespera/fork)
[![GitHub issues](https://img.shields.io/github/issues/dev-five-git/vespera.svg)](https://github.com/dev-five-git/vespera/issues)
[![GitHub pull requests](https://img.shields.io/github/issues-pr/dev-five-git/vespera.svg)](https://github.com/dev-five-git/vespera/pulls)
[![GitHub last commit](https://img.shields.io/github/last-commit/dev-five-git/vespera.svg)](https://github.com/dev-five-git/vespera/commits/main)
[![OpenAPI](https://img.shields.io/badge/OpenAPI-3.1-green.svg)](https://www.openapis.org/)

---

## Introduction

Vespera is a fully automated OpenAPI engine for Axum — delivering a FastAPI-like developer experience to the Rust ecosystem.

It automatically discovers routes, imports handlers and schemas, and generates a complete OpenAPI 3.1 specification with zero configuration.

Just write your Axum API.  
Vespera handles the rest.

---

## Features

### 1. Zero-Config Route Discovery
Automatically scans Axum routers and submodules to detect all registered routes.

### 2. Auto-Import of Handlers and Schemas
Automatically pulls in handlers, request/response types, and data models into the OpenAPI spec.

### 3. Fully Automated OpenAPI Engine
Generates a complete OpenAPI 3.1 document from:
- Routes
- Extractors
- Parameters
- Request bodies
- Response bodies
- Rust data structures (Serde)

### 4. Type-Safe Schema Extraction
Rust types are converted into JSON Schema with full type fidelity.

### 5. Built-in Swagger UI
Automatically generates and serves Swagger UI documentation when `docs_url` is specified, providing interactive API exploration.

### 6. Axum-First Design
Built specifically for Axum's architecture while offering the productivity of modern API frameworks.

---

## Example

### Routes Auto Import

```rust
use axum::{Router, routing::get};
use vespera::vespera;
use axum::Json;

async fn health() -> &'static str {
    "ok"
}

async fn get_user(id: u32) -> Json<User> {
    Json(User { id, name: "Alice".into() })
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    let config = Config::from_env();
    let port = config.port;
    let db = create_db_connection(&config.database_url).await;

    let state = AppState { db, config };

    let app = vespera!(
        openapi = "openapi.json",
        title = "My API",
        version = "1.0.0",
        docs_url = "/docs"
    )
    .with_state(state)
    .layer(
        CorsLayer::new()
            .allow_origin("http://localhost:3000".parse::<HeaderValue>().unwrap())
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ]),
    );

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("API server is running on port {}", port);
    println!("Swagger UI available at http://localhost:{}/docs", port);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

---

## Installation

Add the following to your `Cargo.toml`:

```toml
[dependencies]
vespera = "0.1.0"
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

---

## Quick Start

### 1. Create Project Structure

Create a `src/routes` folder in your project root and write route handlers:

```
src/
├── main.rs
└── routes/
    ├── mod.rs
    ├── users.rs
    └── posts.rs
```

### 2. Write Route Handlers

`src/routes/users.rs`:

```rust
use axum::{Json, Path, State};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct User {
    pub id: u32,
    pub name: String,
}

/// `/users/{id}` - default method is get
#[vespera::route(path = "/{id}")]
pub async fn get_user(
    State(state): State<AppState>,
    Path(id): Path<i32>) -> Json<User> {
    Json(User {
        id,
        name: "Alice".into(),
    })
}

/// /users
#[vespera::route(method = "post")]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    Json(user)
}
```

### 3. Register Modules

`src/routes/mod.rs`:

```rust
pub mod users;
pub mod posts;
```

### 4. Setup Main Application

`src/main.rs`:

```rust
use vespera::vespera;

#[tokio::main]
async fn main() {
    // Basic usage: scans "routes" folder by default
    let app = vespera!();
    
    // Or with OpenAPI and Swagger UI support
    let app = vespera!(
        openapi = "openapi.json",
        title = "My API",
        version = "1.0.0",
        docs_url = "/docs"
    );
    
    // Or specify a custom folder: vespera!("api")
    
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Server running on http://localhost:3000");
    println!("Swagger UI available at http://localhost:3000/docs");
    axum::serve(listener, app).await.unwrap();
}
```

---

## Usage

### `vespera!` Macro

Automatically scans routes and generates an Axum Router with optional OpenAPI and Swagger UI support.

```rust
// Basic usage: scans "routes" folder
let app = vespera!();

// Specify a custom folder
let app = vespera!("api");

// With OpenAPI JSON file generation
let app = vespera!(
    openapi = "openapi.json"
);

// Generate multiple OpenAPI JSON files at once
let app = vespera!(
    openapi = ["openapi.json", "admin-openapi.json"]
);

// With OpenAPI and Swagger UI
let app = vespera!(
    openapi = "openapi.json",
    docs_url = "/docs"
);

// Full configuration with all parameters
let app = vespera!(
    dir = "routes",              // Route folder name (default: "routes")
    openapi = "openapi.json",    // OpenAPI JSON file path (optional)
    title = "My API",            // API title (optional, default: "API")
    version = "1.0.0",           // API version (optional, default: Cargo.toml version)
    docs_url = "/docs"           // Swagger UI documentation URL (optional)
);
```

#### Parameter Description

- **`dir`**: Route folder name (default: `"routes"`)
  - You can also specify it directly as a string literal: `vespera!("api")`
  
- **`openapi`**: OpenAPI JSON file path(s) (optional)
  - Accepts a single string or an array of strings
  - If specified, an OpenAPI 3.1 spec is generated at compile time and **writes an `openapi.json` file to the specified path (or paths)**
  - Example: `openapi = "openapi.json"` → creates `openapi.json` file in project root
  - Example: `openapi = "docs/api.json"` → creates `docs/api.json` file
  - Example: `openapi = ["openapi.json", "docs/admin.json"]` → writes both files
  
- **`title`**: API title (optional, default: `"API"`)
  - Used in the `info.title` field of the OpenAPI document
  
- **`version`**: API version (optional, default: your `Cargo.toml` version)
  - Used in the `info.version` field of the OpenAPI document
  - If not specified, automatically uses the version from your project's `Cargo.toml` (`CARGO_PKG_VERSION`)
  
- **`docs_url`**: Swagger UI documentation URL (optional)
  - If specified, you can view the API documentation through Swagger UI at that path
  - Example: Setting `docs_url = "/docs"` allows viewing documentation at `http://localhost:3000/docs`

- **`redoc_url`**: ReDoc documentation URL (optional)
  - If specified, you can view the API documentation through ReDoc at that path
  - Example: Setting `redoc_url = "/redoc"` allows viewing documentation at `http://localhost:3000/redoc`

#### Environment Variables

All macro parameters can also be configured via environment variables. Environment variables are used as fallbacks when the corresponding macro parameter is not specified.

| Macro Parameter | Environment Variable | Description |
|-----------------|---------------------|-------------|
| `dir` | `VESPERA_DIR` | Route folder name |
| `openapi` | `VESPERA_OPENAPI` | OpenAPI JSON file path |
| `title` | `VESPERA_TITLE` | API title |
| `version` | `VESPERA_VERSION` | API version |
| `docs_url` | `VESPERA_DOCS_URL` | Swagger UI documentation URL |
| `redoc_url` | `VESPERA_REDOC_URL` | ReDoc documentation URL |

**Priority Order** (highest to lowest):
1. Macro parameter (e.g., `version = "1.0.0"`)
2. Environment variable (e.g., `VESPERA_VERSION`)
3. `CARGO_PKG_VERSION` (for `version` only)
4. Default value

**Example:**

```bash
# Set environment variables
export VESPERA_TITLE="My Production API"
export VESPERA_VERSION="2.0.0"
export VESPERA_DOCS_URL="/api-docs"
```

```rust
// These will use the environment variables as defaults
let app = vespera!(
    openapi = "openapi.json"
);
```

### `#[route]` Attribute Macro

Specify HTTP method and path for handler functions.

```rust
// GET request
#[vespera::route(get)]
pub async fn list_users() -> Json<Vec<User>> {
    // ...
}

// POST request (custom path)
#[vespera::route(post, path = "/users")]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    // ...
}

// Path parameter support
#[vespera::route(get, path = "/users/:id")]
pub async fn get_user(id: u32) -> Json<User> {
    // ...
}
```

### Tags and Description

You can add tags and descriptions to your routes for better OpenAPI documentation organization.

#### Tags

Use the `tags` parameter to group your routes in the OpenAPI documentation:

```rust
#[vespera::route(get, tags = ["users"])]
pub async fn list_users() -> Json<Vec<User>> {
    // ...
}

#[vespera::route(post, tags = ["users", "admin"])]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    // ...
}
```

#### Description

There are two ways to add descriptions to your routes:

**1. Using doc comments (recommended):**

Doc comments (`///`) are automatically extracted and used as the route description in OpenAPI:

```rust
/// Get all users from the database
///
/// Returns a list of all registered users.
#[vespera::route(get)]
pub async fn list_users() -> Json<Vec<User>> {
    // ...
}
```

**2. Using the `description` parameter:**

You can also explicitly set the description using the `description` parameter. This takes priority over doc comments:

```rust
/// This doc comment will be ignored
#[vespera::route(get, description = "Custom description for OpenAPI")]
pub async fn list_users() -> Json<Vec<User>> {
    // ...
}
```

#### Combined Example

```rust
/// Get user by ID
///
/// Retrieves a specific user by their unique identifier.
#[vespera::route(get, path = "/{id}", tags = ["users"])]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    // ...
}

#[vespera::route(post, tags = ["users", "admin"], description = "Create a new user account")]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    // ...
}
```

### Supported HTTP Methods

- `GET`
- `POST`
- `PUT`
- `PATCH`
- `DELETE`
- `HEAD`
- `OPTIONS`

### OpenAPI JSON Generation and Swagger UI

When you specify the `openapi` parameter in the `vespera!` macro, an OpenAPI 3.1 spec is automatically generated at compile time and **writes a file to the specified path**.

```rust
let app = vespera!(
    openapi = "openapi.json",    // Creates openapi.json file at this path
    title = "My API",            // API title
    version = "1.0.0",           // API version
    docs_url = "/docs"           // Swagger UI URL (optional)
);
```

With this configuration:
- An OpenAPI JSON file is automatically generated at the specified path during compilation
  - `openapi = "openapi.json"` → creates `openapi.json` file in project root
  - `openapi = "docs/api.json"` → creates `docs/api.json` file
- If you specify `docs_url`, you can view the API documentation through Swagger UI at that path
- The OpenAPI spec is automatically generated by analyzing routes, handlers, and request/response types

**Note**: The `build.rs` file is no longer needed. The `vespera!` macro automatically handles it at compile time.

### File Structure and Route Mapping

File structure is automatically converted to URL paths:

```
routes/
├── users.rs          → /users
├── posts.rs          → /posts
└── admin/
    └── users.rs      → /admin/users
```

---

## Project Structure

```
vespera/
├── Cargo.toml
├── README.md
└── crates/
    └── vespera/
        ├── Cargo.toml
        └── src/
            ├── lib.rs          # Main macro definitions
            ├── args.rs         # Macro argument parsing
            ├── file_utils.rs   # File system utilities
            ├── method.rs       # HTTP method definitions
            └── route/
                ├── mod.rs
                └── utils.rs    # Route information extraction
```

---

## How It Works

1. **Compile-Time Scanning**: The `vespera!` macro scans the specified folder to discover all route handlers.

2. **Attribute Parsing**: Extracts HTTP method and path information from each handler's `#[route]` attribute.

3. **Code Generation**: Automatically generates Axum Router code based on discovered routes.

4. **Type Safety**: Leverages Rust's type system to ensure all routes are correctly registered at compile time.

---

## Contributing

Contributions are welcome! Please open an issue or submit a Pull Request.

### Development Setup

```bash
# Clone the repository
git clone https://github.com/yourusername/vespera.git
cd vespera

# Build
cargo build

# Run tests
cargo test
```

---

## License

This project is licensed under the Apache 2.0 License. See the `LICENSE` file for details.

---

## Roadmap

- [x] Automatic routes importing
- [x] Automatic OpenAPI 3.1 spec generation (via `vespera!` macro)
- [x] Automatic request/response schema extraction
- [x] Swagger UI integration
- [ ] Support for more Axum extractors

---

## Acknowledgments

Vespera is inspired by FastAPI’s developer experience and also takes inspiration from Next.js, all designed for the Rust ecosystem.
