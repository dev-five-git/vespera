# Vespera
A fully automated OpenAPI engine for Axum with zero-config route and schema discovery.

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

### 5. Axum-First Design
Built specifically for Axum’s architecture while offering the productivity of modern API frameworks.

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

    let app = vespera!();

    // route auto import
    let openapi = vespera_openapi!().with_state(state).layer(
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

    let openapi = vespera_openapi!();
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("API server is running on port {}", port);
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

/// `/users/{id}`
#[vespera::route(method = "get", path = "/{id}")]
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
    let app = vespera!(); // Scans "routes" folder by default
    // Or specify a custom folder: vespera!("api")
    
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Server running on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
```

---

## Usage

### `vespera!` Macro

Automatically scans routes and generates an Axum Router.

```rust
// Basic usage: scans "routes" folder
let app = vespera!();

// Specify a custom folder
let app = vespera!("api");
```

### `#[route]` Attribute Macro

Specify HTTP method and path for handler functions.

```rust
// GET request
#[vespera::route(method = "get")]
pub async fn list_users() -> Json<Vec<User>> {
    // ...
}

// POST request (custom path)
#[vespera::route(method = "post", path = "/users")]
pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    // ...
}

// Path parameter support
#[vespera::route(method = "get", path = "/users/:id")]
pub async fn get_user(id: u32) -> Json<User> {
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

- [v] Automatic routes importing
- [ ] Automatic OpenAPI 3.1 spec generation (`vespera_openapi!` macro)
- [ ] Automatic request/response schema extraction
- [ ] Swagger UI integration
- [ ] Support for more Axum extractors

---

## Acknowledgments

Vespera is inspired by FastAPI’s developer experience and also takes inspiration from Next.js, all designed for the Rust ecosystem.
