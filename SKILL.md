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
| `FieldData<NamedTempFile>` | `string` + `format: binary` | File upload field |
| `()` | empty response | 204 No Content |
| Custom struct | `$ref` | Must derive Schema |

## Extractor Mapping Reference

| Axum Extractor | OpenAPI Location | Notes |
|----------------|------------------|-------|
| `Path<T>` | path parameter | T can be tuple or struct |
| `Query<T>` | query parameters | Struct fields become params |
| `Json<T>` | requestBody | application/json |
| `Form<T>` | requestBody | application/x-www-form-urlencoded |
| `TypedMultipart<T>` | requestBody | multipart/form-data — typed with schema |
| `Multipart` | requestBody | multipart/form-data — untyped, generic object |
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

## schema_type! Macro (RECOMMENDED)

> **ALWAYS prefer `schema_type!` over manually defining request/response structs.**
> 
> Benefits:
> - Single source of truth (your model)
> - Auto-generated `From` impl for easy conversion
> - Automatic type resolution (enums, custom types → absolute paths)
> - SeaORM relation support (HasOne, BelongsTo, HasMany)
> - No manual field synchronization

### Best Practices

| DO | DON'T |
|----|-------|
| Use `pick` to select only needed fields | Define manual structs that duplicate Model fields |
| Use `omit` to exclude sensitive fields | Use `name` parameter unnecessarily |
| Use full `crate::models::...` paths | Rely on implicit module resolution |
| Define schema near route handlers | Scatter schemas across unrelated files |

**Primary Parameters (USE THESE):**
- `pick = [...]` - Allowlist: include ONLY these fields
- `omit = [...]` - Denylist: exclude these fields

**Advanced Parameters (USE SPARINGLY):**
- `partial` - For PATCH endpoints only
- `rename` - Only when API naming differs from model
- `add` - Only when truly new fields needed (breaks `From` impl)
- `name` - **AVOID** unless same-file Model reference (see below)

### Why Not Manual Structs?

```rust
// ❌ BAD: Manual struct definition - requires sync with Model
#[derive(Serialize, Deserialize, Schema)]
pub struct UserResponse {
    pub id: i32,
    pub name: String,
    pub email: String,
    // Forgot to add new field? Schema out of sync!
}

// ✅ GOOD: Derive from Model - always in sync
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);
```

### Basic Syntax

```rust
// Pick specific fields
schema_type!(CreateUserRequest from crate::models::user::Model, pick = ["name", "email"]);

// Omit specific fields
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash", "internal_id"]);

// Add new fields (NOTE: no From impl generated when using add)
schema_type!(UpdateUserRequest from crate::models::user::Model, pick = ["name"], add = [("id": i32)]);

// Rename fields
schema_type!(UserDTO from crate::models::user::Model, rename = [("id", "user_id")]);

// Partial updates (all fields become Option<T>)
schema_type!(UserPatch from crate::models::user::Model, partial);

// Partial updates (specific fields only)
schema_type!(UserPatch from crate::models::user::Model, partial = ["name", "email"]);

// Custom serde rename strategy
schema_type!(UserSnakeCase from crate::models::user::Model, rename_all = "snake_case");

// Custom OpenAPI schema name
schema_type!(Schema from Model, name = "UserSchema");

// Skip Schema derive (won't appear in OpenAPI)
schema_type!(InternalDTO from Model, ignore);

// Disable Clone derive
schema_type!(LargeResponse from SomeType, clone = false);
```

### Same-File Model Reference (When to Use `name`)

> **The `name` parameter is ONLY needed for same-file Model references.**
> For cross-file references, use full paths and descriptive struct names instead.

When defining Schema in the same file as Model (common for SeaORM entities):

```rust
// In src/models/user.rs
pub struct Model {
    pub id: i32,
    pub name: String,
    pub status: UserStatus,  // Custom enum - auto-resolved to absolute path
}

pub enum UserStatus { Active, Inactive }

// ✅ CORRECT: Same-file reference - use `name` for OpenAPI schema name
vespera::schema_type!(Schema from Model, name = "UserSchema");

// ❌ WRONG: Using `name` for cross-file reference
// schema_type!(Schema from crate::models::user::Model, name = "UserResponse");
// ✅ CORRECT: Use descriptive struct name instead
// schema_type!(UserResponse from crate::models::user::Model, omit = ["password"]);
```

**Why avoid `name` for cross-file references?**
- The struct name itself becomes the OpenAPI schema name
- `UserResponse` is clearer than `Schema` with `name = "UserResponse"`
- Less parameters = less complexity

### Cross-File References

Reference structs from other files using full module paths:

```rust
// In src/routes/users.rs
use vespera::schema_type;

// Reference model from src/models/user.rs
schema_type!(CreateUserRequest from crate::models::user::Model, pick = ["name", "email"]);
```

The macro reads the source file at compile time - no special annotations needed on the source struct.

### Auto-Generated From Impl

When `add` is NOT used, `schema_type!` generates a `From` impl for easy conversion:

```rust
// This:
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);

// Generates:
pub struct UserResponse { id, name, email, created_at }

impl From<crate::models::user::Model> for UserResponse {
    fn from(source: crate::models::user::Model) -> Self {
        Self { id: source.id, name: source.name, ... }
    }
}

// Usage:
let model: Model = db.find_user(id).await?;
Json(model.into())  // Easy conversion!
```

**Note:** `From` is NOT generated when `add` is used (can't auto-populate added fields).

### Parameters

**Recommended (Primary):**

| Parameter | Description | Example |
|-----------|-------------|---------|
| `pick` | Include only these fields | `pick = ["name", "email"]` |
| `omit` | Exclude these fields | `omit = ["password"]` |

**Situational (Use When Needed):**

| Parameter | Description | When to Use |
|-----------|-------------|-------------|
| `partial` | Make fields optional | PATCH endpoints only |
| `rename` | Rename fields | API naming differs from model |
| `rename_all` | Serde rename strategy | Different casing needed |
| `add` | Add new fields | New fields not in model (breaks `From` impl) |
| `multipart` | Derive `TryFromMultipart` | Multipart form-data endpoints |

**Avoid (Special Cases Only):**

| Parameter | Description | When to Use |
|-----------|-------------|-------------|
| `name` | Custom OpenAPI schema name | **Same-file Model reference only** |
| `ignore` | Skip Schema derive | Internal DTOs not for OpenAPI |
| `clone` | Control Clone derive | Large structs where Clone is expensive |

### SeaORM Integration (RECOMMENDED)

`schema_type!` has first-class SeaORM support with automatic relation handling:

```rust
// src/models/memo.rs
#[derive(Clone, Debug, DeriveEntityModel)]
#[sea_orm(table_name = "memo")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    pub status: MemoStatus,                      // Custom enum
    pub user: BelongsTo<super::user::Entity>,    // → Option<Box<UserSchema>>
    pub comments: HasMany<super::comment::Entity>, // → Vec<CommentSchema>
    pub created_at: DateTimeWithTimeZone,        // → chrono::DateTime<FixedOffset>
}

#[derive(EnumIter, DeriveActiveEnum, Serialize, Deserialize, Schema)]
pub enum MemoStatus { Draft, Published, Archived }

// Generates Schema with proper types - no imports needed!
vespera::schema_type!(Schema from Model, name = "MemoSchema");
```

**Automatic Type Conversions:**

| SeaORM Type | Generated Type | Notes |
|-------------|---------------|-------|
| `HasOne<Entity>` | `Box<Schema>` or `Option<Box<Schema>>` | Based on FK nullability |
| `BelongsTo<Entity>` | `Option<Box<Schema>>` | Always optional |
| `HasMany<Entity>` | `Vec<Schema>` | |
| `DateTimeWithTimeZone` | `vespera::chrono::DateTime<FixedOffset>` | No SeaORM import needed |
| Custom enums | `crate::module::EnumName` | Auto-resolved to absolute path |

**Circular Reference Handling:** Automatically detected and handled by inlining fields.

### Complete Example

```rust
// ============================================
// src/models/user.rs (SeaORM entity)
// ============================================
#[derive(Clone, Debug, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub email: String,
    pub status: UserStatus,
    pub password_hash: String,  // Never expose!
    pub created_at: DateTimeWithTimeZone,
}

// ✅ Same-file: use `name` parameter for OpenAPI schema name
vespera::schema_type!(Schema from Model, name = "UserSchema");

// ============================================
// src/routes/users.rs (Route handlers)
// ============================================
use vespera::schema_type;

// ✅ Cross-file: use descriptive struct names + pick/omit
// NO `name` parameter needed - struct name = OpenAPI schema name
schema_type!(CreateUserRequest from crate::models::user::Model, pick = ["name", "email"]);
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);
schema_type!(UserPatch from crate::models::user::Model, omit = ["password_hash", "id"], partial);

#[vespera::route(get, path = "/{id}")]
pub async fn get_user(Path(id): Path<i32>, State(db): State<DbPool>) -> Json<UserResponse> {
    let user = User::find_by_id(id).one(&db).await.unwrap().unwrap();
    Json(user.into())  // From impl handles conversion
}

#[vespera::route(patch, path = "/{id}")]
pub async fn patch_user(
    Path(id): Path<i32>,
    Json(patch): Json<UserPatch>,  // All fields are Option<T>
) -> Json<UserResponse> {
    // Apply partial update...
}
```

### Multipart Mode (`multipart`)

Generate `TryFromMultipart` structs from existing multipart request types:

```rust
use axum_typed_multipart::{FieldData, TryFromMultipart, TypedMultipart};
use tempfile::NamedTempFile;

// Base multipart struct (manually defined)
#[derive(TryFromMultipart, vespera::Schema)]
pub struct CreateUploadRequest {
    pub name: String,
    #[form_data(limit = "10MiB")]
    pub thumbnail: Option<FieldData<NamedTempFile>>,
    #[form_data(limit = "50MiB")]
    pub document: Option<FieldData<NamedTempFile>>,
    pub tags: Option<String>,
}

// Derive a partial update struct via schema_type!
// - Derives TryFromMultipart (not serde)
// - All fields become Option<T> (partial)
// - "document" field excluded
// - #[form_data(limit = "10MiB")] preserved from source
schema_type!(PatchUploadRequest from CreateUploadRequest, multipart, partial, omit = ["document"]);
```

**What `multipart` mode changes:**

| Aspect | Normal Mode | Multipart Mode |
|--------|------------|----------------|
| Derives | `Serialize`, `Deserialize` | `TryFromMultipart` |
| Struct attrs | `#[serde(rename_all=...)]` | None |
| Field attrs | `#[serde(...)]` preserved | `#[form_data(...)]` preserved |
| Relation fields | Included (BelongsTo/HasOne) | **Skipped** (can't represent in forms) |
| `From` impl | Auto-generated | **Not generated** |

**OpenAPI rename alignment:** The schema parser reads `#[form_data(field_name = "...")]` and `#[try_from_multipart(rename_all = "...")]` as fallbacks when serde attrs are absent, ensuring OpenAPI field names match runtime multipart parsing.

**Dependencies required in your Cargo.toml:**
```toml
axum = "0.8"                   # Required: TryFromMultipart references axum internals
axum_typed_multipart = "0.16"  # The multipart crate
tempfile = "3"                 # For NamedTempFile file uploads
```

### Quick Reference

```rust
// ✅ RECOMMENDED PATTERNS
schema_type!(CreateUserRequest from crate::models::user::Model, pick = ["name", "email"]);
schema_type!(UserResponse from crate::models::user::Model, omit = ["password_hash"]);
schema_type!(UserListItem from crate::models::user::Model, pick = ["id", "name"]);

// ✅ MULTIPART PATTERNS
schema_type!(PatchUpload from CreateUploadRequest, multipart, partial);
schema_type!(SmallUpload from CreateUploadRequest, multipart, omit = ["document"]);

// ⚠️ USE SPARINGLY
schema_type!(UserPatch from crate::models::user::Model, partial);  // PATCH only
schema_type!(Schema from Model, name = "UserSchema");              // Same-file only

// ❌ AVOID
schema_type!(Schema from crate::models::user::Model, name = "UserResponse");  // Use struct name!
```

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
