---
name: vespera
description: Build APIs with Vespera - FastAPI-like DX for Rust/Axum. Covers route handlers, Schema derivation, and OpenAPI generation.
---

# Vespera Usage Guide

Vespera = FastAPI DX for Rust. Zero-config OpenAPI 3.1 generation via compile-time macro scanning.

## Quick Start

```rust
// 1. Main entry - vespera! macro handles everything
let app = vespera!(
    openapi = "openapi.json",  // writes file at compile time
    title = "My API",
    version = "1.0.0",
    docs_url = "/docs",        // Swagger UI
    redoc_url = "/redoc"       // ReDoc alternative
);

// 2. Route handlers - MUST be pub async fn
#[vespera::route(get, path = "/{id}", tags = ["users"])]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> { ... }

// 3. Custom types - derive Schema for OpenAPI inclusion
#[derive(Serialize, Deserialize, vespera::Schema)]
pub struct User { id: u32, name: String }
```

---

## Type Mapping Reference

| Rust Type | OpenAPI Schema | Notes |
|-----------|----------------|-------|
| `String`, `&str` | `string` | |
| `i8`-`i128`, `u8`-`u128` | `integer` | |
| `f32`, `f64` | `number` | |
| `bool` | `boolean` | |
| `Vec<T>` | `array` + items | |
| `Option<T>` | T (nullable context) | Parent marks as optional |
| `HashMap<K,V>` | `object` + additionalProperties | |
| `()` | empty response | 204 No Content |
| Custom struct | `$ref` | Must derive Schema |

## Extractor Mapping Reference

| Axum Extractor | OpenAPI Location | Notes |
|----------------|------------------|-------|
| `Path<T>` | path parameter | T can be tuple or struct |
| `Query<T>` | query parameters | Struct fields become params |
| `Json<T>` | requestBody | application/json |
| `Form<T>` | requestBody | application/x-www-form-urlencoded |
| `State<T>` | **ignored** | Internal, not API |
| `Extension<T>` | **ignored** | Internal, not API |
| `TypedHeader<T>` | header parameter | |
| `HeaderMap` | **ignored** | Too dynamic |

---

## Route Handler Requirements

```rust
// ❌ Private function - NOT discovered
async fn get_users() -> Json<Vec<User>> { ... }

// ❌ Non-async function - NOT supported
pub fn get_users() -> Json<Vec<User>> { ... }

// ✅ Must be pub async fn
pub async fn get_users() -> Json<Vec<User>> { ... }
```

---

## File Structure → URL Mapping

```
src/routes/
├── mod.rs           → /              (root routes)
├── users.rs         → /users
├── posts.rs         → /posts
└── admin/
    ├── mod.rs       → /admin
    └── stats.rs     → /admin/stats
```

Handler path is: `{file_path} + {#[route] path}`

```rust
// In src/routes/users.rs
#[vespera::route(get, path = "/{id}")]
pub async fn get_user(...) // → GET /users/{id}
```

---

## Serde Integration

Vespera respects serde attributes:

```rust
#[derive(Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]  // ✅ Respected in schema
pub struct UserResponse {
    user_id: u32,        // → "userId" in JSON Schema
    
    #[serde(rename = "fullName")]  // ✅ Respected
    name: String,        // → "fullName" in JSON Schema
    
    #[serde(default)]    // ✅ Marks as optional in schema
    bio: Option<String>,
    
    #[serde(skip)]       // ✅ Excluded from schema
    internal_id: u64,
}
```

---

## Debugging Tips

### Schema Not Appearing

1. Check `#[derive(Schema)]` on the type
2. Check type is used in a route handler's input/output
3. Check for generic types - all type params need Schema

```rust
// Generic types need Schema on all params
#[derive(Schema)]
struct Paginated<T: Schema> {  // T must also derive Schema
    items: Vec<T>,
    total: u32,
}
```

### Macro Expansion

```bash
# See what vespera! generates
cargo expand

# Validate OpenAPI output
npx @apidevtools/swagger-cli validate openapi.json
```

---

## Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `VESPERA_DIR` | Route folder name | `routes` |
| `VESPERA_OPENAPI` | OpenAPI output path | none |
| `VESPERA_TITLE` | API title | `API` |
| `VESPERA_VERSION` | API version | `CARGO_PKG_VERSION` |
| `VESPERA_DOCS_URL` | Swagger UI path | none |
| `VESPERA_REDOC_URL` | ReDoc path | none |
| `VESPERA_SERVER_URL` | Server URL | `http://localhost:3000` |

---

## Merging Multiple Vespera Apps

Combine routes and OpenAPI specs from multiple apps at compile time.

### export_app! Macro

Export an app for merging:

```rust
// Child crate (e.g., third/src/lib.rs)
mod routes;

// Basic - scans "routes" folder by default
vespera::export_app!(ThirdApp);

// Custom directory
vespera::export_app!(ThirdApp, dir = "api");
```

Generates:
- `ThirdApp::OPENAPI_SPEC: &'static str` - OpenAPI JSON
- `ThirdApp::router() -> Router` - Axum router

### merge Parameter

Merge child apps in parent:

```rust
let app = vespera!(
    openapi = "openapi.json",
    docs_url = "/docs",
    merge = [third::ThirdApp, other::OtherApp]
)
.with_state(state);
```

**What happens:**
1. Child routers merged into parent router
2. OpenAPI specs merged (paths, schemas, tags)
3. Swagger UI shows all routes

### How It Works (Compile-Time)

```
Child compilation (export_app!):
  1. Scan routes/ folder
  2. Generate OpenAPI spec
  3. Write to target/vespera/{Name}.openapi.json

Parent compilation (vespera! with merge):
  1. Generate parent OpenAPI spec
  2. Read child specs from target/vespera/
  3. Merge all specs together
  4. Write merged openapi.json
```
