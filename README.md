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
| Schema extraction | From Rust types (`#[derive(Schema)]`) | Manual JSON Schema |
| **Request validation** | **`Validated<T>` extractor → auto `422`** | Manual checks in every handler |
| **Server startup** | **`.serve("0.0.0.0:3000")` one-liner** | `TcpListener::bind` + `axum::serve` |
| Swagger UI | Built-in | Separate setup |
| Type safety | Compile-time verified | Runtime errors |

## Quick Start

### 1. Add Dependencies

```toml
[dependencies]
vespera = "0.6"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

> Vespera re-exports axum as `vespera::axum`, so you do not need a direct
> `axum` dependency (the re-export keeps the version in sync).

> **MSRV:** Rust 1.88 (edition 2024), checked in CI.

### 2. Create Route Handler

```
src/
├── main.rs
└── routes/
    ├── mod.rs
    └── users.rs
```

**`src/routes/mod.rs`**:
```rust
pub mod users;
```

**`src/routes/users.rs`**:
```rust
use serde::{Deserialize, Serialize};
use vespera::Schema;
use vespera::axum::{Json, extract::Path};

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
mod routes;

use vespera::{Serve, vespera};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    println!("Swagger UI: http://localhost:3000/docs");
    vespera!(
        openapi = "openapi.json",
        title = "My API",
        docs_url = "/docs"
    )
    .serve("0.0.0.0:3000")
    .await
}
```

> `.serve(addr)` is a vespera-provided extension trait on `axum::Router` — it
> replaces the usual `TcpListener::bind` + `axum::serve(...)` dance with a
> single chained call. `addr` accepts anything `tokio::net::ToSocketAddrs`
> takes (strings, tuples, `SocketAddr`). Handlers can extract
> `ConnectInfo<SocketAddr>`, and `.serve_with_graceful_shutdown(addr, signal)`
> stops accepting connections once `signal` resolves.

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

`snake_case` file and folder names become `kebab-case` URL segments
(`user_groups.rs` → `/user-groups`). An explicit `#[route(path = "...")]`
suffix is used verbatim — `path = "/my_route"` serves `/my_route`. axum
catch-all segments (`path = "/files/{*rest}"`) are documented as
`/files/{rest}` with a `rest` path parameter.

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
#[vespera::route(
    put,
    path = "/{id}",
    tags = ["users"],
    operation_id = "updateUser",
    summary = "Update a user",
    description = "Update user",
    deprecated
)]
pub async fn update_user(...) -> ... { ... }

// Override or require auth for one operation in OpenAPI
#[vespera::route(get, path = "/me", tags = ["users"], security = ["bearerAuth"])]
pub async fn current_user(...) -> ... { ... }

// Declare headers consumed by custom extractors so they appear in OpenAPI
#[vespera::route(
    get,
    headers = [
        { name = "Authorization", required = true, description = "Bearer token" },
        { name = "X-Trace-Id" }
    ]
)]
pub async fn custom_auth_user(...) -> ... { ... }

// Operation-level examples attach to requestBody / 200 response media types
#[vespera::route(
    post,
    request_example = r#"{"name":"Alice"}"#,
    response_example = r#"{"id":1,"name":"Alice"}"#
)]
pub async fn create_user(...) -> ... { ... }
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

When serde reads a type differently than it writes it — `#[serde(default)]`,
one-sided `skip_deserializing` / `skip_serializing`, or
`rename(deserialize = "..")` — request bodies reference a separate
`{Name}Input` component (defaulted fields optional, `skip_deserializing`
fields absent, deserialize names), FastAPI-style, while responses keep
`{Name}`. Types whose two views are identical get no extra component.
`#[schema(name = "X")]` yields `XInput`; if that name is already taken the
first free `XInput2`, `XInput3`, … is used.

#### Custom serde conversions (`#[schema(value_type = T)]`)

serde attributes that hand a value to your own code change its wire shape,
so vespera never documents the declared Rust type for them:

```rust
#[derive(Serialize, Deserialize, vespera::Schema)]
pub struct Event {
    /// `ts_seconds` writes an integer, not a date-time string.
    #[serde(with = "vespera::chrono::serde::ts_seconds")]
    #[schema(value_type = i64)]
    pub created_at: DateTime<Utc>,
    #[serde(with = "my_hex")]          // no value_type → `{}` + a note
    pub checksum: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, vespera::Schema)]
#[serde(into = "String", try_from = "String")] // documented as `string`
pub struct Rgb { pub r: u8, pub g: u8, pub b: u8 }
```

| Attribute | Documented as |
|---|---|
| field `#[schema(value_type = T)]` | `T`'s schema (constraints such as `minimum` still apply) |
| field `#[serde(with / serialize_with / deserialize_with)]` without `value_type` | `{}` (any value) with a description saying the shape is unknown — never the Rust type. One-sided `serialize_with` / `deserialize_with` only affects that direction (`{Name}Input` for requests); an `Option` read by a custom deserializer is `required` in requests, as in serde |
| container `#[serde(into = "T")]` | responses use `T`; requests keep the fields (`{Name}Input`) |
| container `#[serde(from = "T")]` / `try_from` | requests use `T` (`{Name}Input`); responses keep the fields |
| container `#[schema(value_type = T)]` | `T` in both directions (wins over `into` / `from`) |

The derive's `Schema` assertion checks `T` instead of the field type, and
skips a field whose type is never documented (`with` without `value_type`).
SeaORM's `Json` / `JsonValue` columns are aliases of `serde_json::Value` and
are documented as `{}` (any JSON value, not just objects); a `Json<T>` with a
type argument is axum's wrapper, not a JSON value.

### Request Validation (`Validated<T>` → `422`)

`Validated<T>` wraps any axum extractor (`Json`, `Form`, `Query`, `Path`) and
runs the inner type's `garde::Validate` impl **before** the handler is
called. Validation failures are converted to **`422 Unprocessable Entity`**
with a canonical JSON envelope — no manual error mapping per handler.

The recommended way to declare constraints is `#[schema(...)]` on a
`#[derive(Schema)]` type. The derive generates the validator itself (no
`garde` dependency in your `Cargo.toml`), and the same constraints appear in
the generated OpenAPI schema (`minLength`, `pattern`, `format`, `minimum`, …):

```rust
use serde::Deserialize;
use vespera::{Schema, Validated, axum::Json};

#[derive(Deserialize, Schema)]
pub struct CreateUser {
    #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
    pub username: String,
    #[schema(format = "email")]
    pub email: String,
    #[schema(minimum = 18, maximum = 120)]
    pub age: u8,
}

#[vespera::route(post, tags = ["users"])]
pub async fn create_user(
    Validated(Json(req)): Validated<Json<CreateUser>>,
) -> Json<&'static str> {
    // `req` has already passed validation.
    Json("ok")
}
```

| `#[schema(...)]` key | OpenAPI | Runtime check |
|---|---|---|
| `min_length` / `max_length` | `minLength` / `maxLength` | yes |
| `pattern = "..."` | `pattern` (regex checked at compile time) | yes |
| `format = "email"` / `"url"` / `"uri"` / `"ipv4"` / `"ipv6"` / `"ip"` | `format` | yes |
| `minimum` / `maximum` | `minimum` / `maximum` | yes (inclusive) |
| `min_items` / `max_items` | `minItems` / `maxItems` | yes |
| `dive` | — | validates a nested `Schema` field |
| `exclusive_minimum` / `exclusive_maximum` / `multiple_of` / `unique_items` / `format = "uuid"` | emitted | no (annotation only) |
| `value_type = T` | `T`'s schema (see [Custom serde conversions](#custom-serde-conversions-schemavalue_type--t)) | no |

**Alternative — `garde` directly.** If you need garde rules that
`#[schema(...)]` does not cover (custom validators, contexts), derive
`garde::Validate` yourself. This requires your own dependency on
**garde 0.23** (the version `Validated` is built against):

```toml
[dependencies]
garde = { version = "0.23", features = ["derive", "email"] }
```

```rust
use garde::Validate;

#[derive(serde::Deserialize, vespera::Schema, Validate)]
pub struct CreateUser {
    #[garde(length(min = 3, max = 32))]
    pub username: String,
    #[garde(email)]
    pub email: String,
}
```

Use one approach per type: runtime `#[schema(...)]` constraints *and*
`#[derive(garde::Validate)]` on the same struct both implement
`garde::Validate` and fail to compile (`E0119`).

`#[derive(Schema)]` documents every `#[garde(...)]` rule that has a
deterministic JSON Schema equivalent, so the spec states what the `422`
enforces (object properties and `Query<T>` / `Path<T>` struct parameters alike):

| `#[garde(...)]` rule | OpenAPI |
|---|---|
| `length(chars, min, max)` on a string | `minLength` / `maxLength` |
| `length(min, max)` on a string in the default `simple` (byte) mode, or `bytes` / `graphemes` / `utf16` | only `min = 1` → `minLength: 1` (JSON Schema counts code points; other bounds are skipped) |
| `length(min, max)` on a `Vec` / set / map | `minItems` / `maxItems`, `minProperties` / `maxProperties` |
| `range(min, max)` / `range(equal)` (numeric literals) | `minimum` / `maximum` |
| `email` / `url` / `ipv4` / `ipv6` / `ip` | `format: email` / `uri` / `ipv4` / `ipv6` / `anyOf` of both |
| `pattern("...")` / `prefix("x")` / `suffix("x")` / `contains("x")` / `ascii` | `pattern` (literals escaped; extra patterns as `allOf`) |
| `inner(...)` on `Vec<T>` / `[T; N]` (on `Option<T>`: the value) | the rules on `items` |

Rules on an `Option<T>` field constrain the non-null value. A keyword the
field's `#[schema(...)]` sets wins over the derived one, and type-implied bounds
(`u32` → `minimum: 0`) are only tightened. Not documented (no exact
equivalent): `alphanumeric` (Unicode), `required` (the component also documents
responses), `credit_card`, `phone_number`, `matches`, `custom`, `dive`, `if`,
non-literal arguments (consts, static regexes), and byte / grapheme / UTF-16
length bounds above 1. Prefer `length(chars, ...)` when the bound should
appear in the spec.

**Failure response (`HTTP/1.1 422 Unprocessable Entity`):**

```json
{
  "errors": [
    { "path": "username", "message": "length is lower than 3" },
    { "path": "email",    "message": "not a valid email" }
  ]
}
```

Works with every common extractor — same `422` envelope on the wire:

| Extractor | Validates |
|---|---|
| `Validated<Json<T>>` | JSON body |
| `Validated<Form<T>>` | URL-encoded form body |
| `Validated<Query<T>>` | URL query parameters |
| `Validated<Path<T>>` | Path parameters |

Every route with a `Validated<...>` / `ValidatedWith<...>` argument documents the
`422` response in the OpenAPI spec, wherever the argument sits in the signature
(e.g. after a plain `Path<T>`).

`T` must implement `garde::Validate`. `#[derive(Schema)]` generates that impl
only for a struct with named fields and at least one runtime `#[schema(...)]`
rule. A rule-less struct or an enum inside `Validated` is a compile error at the
argument (`` `T` cannot be validated by `Validated` / `ValidatedWith` ``): add a
rule, derive `garde::Validate` yourself, or take the plain extractor.

Input that is well-formed but does not deserialize into `T` — a wrong JSON
type, a missing field, an unparsable query/form/path value — is reported
through the **same `422` envelope**, with `path` pointing at the offending
field (e.g. `"age"`, `"tags[1]"`). Malformed JSON syntax (`400`), a missing
or unsupported `Content-Type` (`415`) and body-limit errors keep axum's
default rejection.

#### Context-aware validation (`ValidatedWith<C, T>`)

`ValidatedWith<C, T>` works like `Validated<T>`, but validates with a context
`C` borrowed from the router state. Use it for garde rules declared with
`#[garde(context(C))]`. The state provides the context through
`vespera::ValidationContext<C>`: state that *is* `C` gets this for free,
and app state can expose a field without cloning it per request:

```rust
use std::sync::Arc;
use vespera::{Schema, ValidatedWith, ValidationContext, axum::Json};

pub struct TagPolicy { pub required_prefix: String }
pub struct AppState { pub tag_policy: TagPolicy }

impl ValidationContext<TagPolicy> for Arc<AppState> {
    fn validation_context(&self) -> &TagPolicy { &self.tag_policy }
}

#[derive(serde::Deserialize, Schema, garde::Validate)]
#[garde(context(TagPolicy as policy))]
pub struct TaggedPost {
    #[garde(custom(|tags: &Vec<String>, policy: &TagPolicy| {
        if tags.iter().all(|t| t.starts_with(&policy.required_prefix)) { Ok(()) }
        else { Err(garde::Error::new("bad tag prefix")) }
    }))]
    pub tags: Vec<String>,
}

#[vespera::route(post)]
pub async fn create_post(post: ValidatedWith<TagPolicy, Json<TaggedPost>>) -> &'static str {
    let Json(_post) = post.into_inner();
    "ok"
}
```

`ValidatedWith` wraps the same extractors as `Validated` and answers the same
`422` envelope. The spec documents the wrapped body or query parameters plus the
`422` response. The context never appears in the spec. Keep `#[schema(...)]` rules off a type that
derives `garde::Validate` itself (`E0119`).

Under JNI, the same `422` body is **hoisted** into the binary wire header as
`"validation_errors": [...]` — Java decoders consume validation errors
without parsing the body. See [`crates/vespera/tests/jni_validation.rs`](./crates/vespera/tests/jni_validation.rs).

### Security Schemes

Declare OpenAPI security schemes in `vespera!`, then attach requirements to
routes with `security = [...]`. Each route entry becomes an OpenAPI security
requirement object with empty scopes; use `security = []` on a route to emit an
explicit unauthenticated operation.

```rust
let app = vespera!(
    openapi = "openapi.json",
    docs_url = "/docs",
    security_schemes = [
        { name = "bearerAuth", type = "http", scheme = "bearer", bearer_format = "JWT" },
        { name = "apiKey", type = "apiKey", in = "header", header_name = "X-API-Key" },
        { name = "basicAuth", type = "http", scheme = "basic" }
    ],
    security = ["bearerAuth"] // optional document-level default
);

#[vespera::route(get, path = "/me", security = ["bearerAuth"])]
pub async fn current_user(...) -> ... { ... }

#[vespera::route(get, path = "/health", security = [])]
pub async fn health() -> &'static str { "ok" }
```

Supported `type` values match OpenAPI's camelCase wire names: `"apiKey"`,
`"http"`, `"mutualTLS"`, `"oauth2"`, and `"openIdConnect"`. The DSL uses
`header_name` for the OpenAPI api-key `name` field so it does not conflict with
the security scheme entry name.

### Supported Extractors

| Extractor | OpenAPI Mapping |
|-----------|-----------------|
| `Path<T>` | Path parameters |
| `Query<T>` | Query parameters |
| `Json<T>` | Request body (application/json) |
| `Form<T>` | Request body (application/x-www-form-urlencoded) |
| `TypedMultipart<T>` | Request body (multipart/form-data) — typed with schema |
| `Multipart` | Request body (multipart/form-data) — untyped, generic object |
| `TypedHeader<T>` | Header parameters |
| `State<T>` | Ignored (internal) |

### Multipart Form Data

#### Typed Multipart (Recommended)

Upload files using vespera's built-in `TypedMultipart` extractor:

```rust
use vespera::multipart::{FieldData, TypedMultipart};
use vespera::{Multipart, Schema};
use tempfile::NamedTempFile;

#[derive(Multipart, Schema)]
pub struct CreateUploadRequest {
    pub name: String,
    #[form_data(limit = "10MiB")]
    pub file: Option<FieldData<NamedTempFile>>,
}

#[vespera::route(post, tags = ["uploads"])]
pub async fn create_upload(
    TypedMultipart(req): TypedMultipart<CreateUploadRequest>,
) -> Json<UploadResponse> { ... }
```

Vespera automatically generates `multipart/form-data` content type in OpenAPI, and maps `FieldData<NamedTempFile>` to `{ "type": "string", "format": "binary" }`.

#### Raw Multipart (Untyped)

For dynamic multipart handling where the fields aren't known at compile time, use axum's built-in `Multipart` extractor:

```rust
use axum::extract::Multipart;

#[vespera::route(post, tags = ["uploads"])]
pub async fn upload(mut multipart: Multipart) -> Json<UploadResponse> {
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or("unknown").to_string();
        let data = field.bytes().await.unwrap();
        // Process each field dynamically...
    }
    Json(UploadResponse { success: true })
}
```

This generates a `multipart/form-data` request body with a generic `{ "type": "object" }` schema in OpenAPI, since the fields are not statically known.

### Error Handling

```rust
#[derive(Serialize, Schema)]
pub struct NotFoundError {
    pub message: String,
}

#[vespera::route(get, path = "/{id}", responses = [(404, NotFoundError)])]
pub async fn get_user(Path(id): Path<u32>) -> Result<Json<User>, (StatusCode, Json<NotFoundError>)> {
    if id == 0 {
        return Err((StatusCode::NOT_FOUND, Json(NotFoundError { message: "Not found".into() })));
    }
    Ok(Json(User { id, name: "Alice".into() }))
}
```

Use `responses = [(status, Type)]` to document typed error bodies. `Type` may be
a bare type name or a path such as `crate::errors::NotFoundError`; Vespera uses
the last path segment as the OpenAPI schema name and emits a JSON `$ref` under
that status. `error_status = [400, 404]` remains available for schema-less extra
error statuses; when both are present, a typed `responses` entry wins for the
same status code.

#### Explicit error responses are authoritative (no auto-`400`)

By default a handler returning `Result<_, E>` (or `Result<_, (StatusCode, E)>`)
infers a single `400` error response, because the macro cannot read the runtime
`StatusCode`. **As soon as you declare any explicit error response** — via
`responses = [(code, Type)]` and/or `error_status = [code, ...]` — that explicit
set becomes authoritative and the inferred `400` is dropped (the success
response is untouched), *unless* `400` is itself among the declared codes:

```rust
// Handler returns (StatusCode::INTERNAL_SERVER_ERROR, Json<ErrorResponse>).
// Declaring responses = [(500, ...)] yields exactly { 200, 500 } — no spurious 400.
#[vespera::route(responses = [(500, ErrorResponse)])]
pub async fn fail() -> Result<&'static str, (StatusCode, Json<ErrorResponse>)> { ... }
```

A plain `Result<_, E>` with **no** error annotations keeps the inferred `400`,
so existing routes are unaffected.

#### Non-`200` success status (`status = <u16>`)

Use `status = <u16>` to override the inferred `200` success key with any `2xx`
code (a non-`2xx` value is a compile error). No-body statuses (`204`, `304`)
emit a success response with no `content`:

```rust
// 204 success + 404 error (text/plain) — no 200, no 400.
#[vespera::route(delete, path = "/{id}", status = 204, error_status = [404])]
pub async fn delete_item(Path(id): Path<i64>) -> Result<StatusCode, (StatusCode, String)> { ... }
```

---

## `vespera!` Macro Reference

```rust
let app = vespera!(
    dir = "routes",                    // Route folder (default: "routes")
    openapi = "openapi.json",          // Output path, relative to the crate dir (written at compile time)
    title = "My API",                  // OpenAPI info.title
    version = "1.0.0",                 // OpenAPI info.version (default: CARGO_PKG_VERSION)
    description = "Pet store API",     // OpenAPI info.description (default: Cargo.toml description)
    summary = "Pets",                  // OpenAPI info.summary
    terms_of_service = "https://example.com/terms",  // info.termsOfService (http(s) URL)
    contact_name = "API Team",         // info.contact.name
    contact_url = "https://example.com/support",     // info.contact.url (http(s) URL)
    contact_email = "api@example.com", // info.contact.email
    license = "MIT OR Apache-2.0",     // info.license.identifier (SPDX); also the name
                                       //   unless license_name is set
    // license_name = "Proprietary", license_url = "https://example.com/license",
                                       //   non-SPDX license: name + url (excludes `license`)
    docs_url = "/docs",                // Swagger UI endpoint
    redoc_url = "/redoc",              // ReDoc endpoint
    spec_url = "/openapi.json",        // Raw OpenAPI JSON endpoint (application/json)
    servers = [                        // OpenAPI servers
        { url = "https://api.example.com", description = "Production" },
        { url = "http://localhost:3000", description = "Development" }
    ],
    security_schemes = [               // OpenAPI components.securitySchemes
        { name = "bearerAuth", type = "http", scheme = "bearer", bearer_format = "JWT" },
        { name = "apiKey", type = "apiKey", in = "header", header_name = "X-API-Key" }
    ],
    security = ["bearerAuth"],         // Optional document-level security
    tags = [                            // OpenAPI top-level tag descriptions
        { name = "users", description = "User operations" },
        { name = "admin", description = "Admin operations" }
    ],
    merge = [crate1::App1, crate2::App2]  // Merge child vespera apps
);
```

### API info

`description`, `summary`, `terms_of_service`, `contact_name`, `contact_url`,
`contact_email`, `license`, `license_name` and `license_url` fill the OpenAPI
3.1 `info` object. They have no environment-variable fallback.

- `license` is an SPDX expression and becomes `info.license.identifier` (and the
  `name`, unless `license_name` overrides it).
- A non-SPDX license uses `license_name` + `license_url`. OpenAPI 3.1 makes
  `identifier` and `url` mutually exclusive, so `license` together with
  `license_url`, or `license_url` without `license_name`, is a compile error.
- `terms_of_service`, `contact_url` and `license_url` must be `http(s)://` URLs
  and `contact_email` must look like an address (compile errors otherwise).
- Without `description`, `info.description` is the crate's `description` from
  `Cargo.toml`.

### Documentation endpoints

`docs_url`, `redoc_url` and `spec_url` are opt-in `GET` routes; each must start
with `/` and they must not share a path (both are compile errors).

- `docs_url` serves Swagger UI and `redoc_url` serves ReDoc. The pages load
  `swagger-ui-dist` / `redoc` from unpkg at **exact pinned versions with
  Subresource Integrity** (`integrity` + `crossorigin="anonymous"`), so a
  tampered CDN file is refused by the browser. The spec is embedded in the page
  and escaped for the `<script>` context, so `</script>` or `<!--` in a
  description or example cannot break the page. The renderers are public as
  `vespera::docs::{swagger_ui_html, redoc_html}` if you want to serve a page
  yourself (pinned versions: `vespera::docs::SWAGGER_UI_VERSION` /
  `REDOC_VERSION`).
- `spec_url` serves the generated OpenAPI document (including `merge`d apps)
  as `application/json` for client generators and API gateways. The docs pages
  keep embedding the spec rather than fetching `spec_url`, so they also work
  when the router is nested under a prefix.

## `#[vespera::route]` Macro Reference

| Parameter | Description |
|-----------|-------------|
| HTTP method | `get`, `post`, `put`, `patch`, `delete`, `head`, or `options` (default: `get`) |
| `path` | Route suffix appended to the file-based path |
| `tags` | OpenAPI operation tags, e.g. `tags = ["users"]` |
| `operation_id` | OpenAPI operationId override, e.g. `operation_id = "getUser"`; defaults to the Rust function name |
| `summary` | OpenAPI operation summary, e.g. `summary = "Get a user"`; otherwise the first paragraph of the doc comment |
| `description` | OpenAPI operation description; otherwise the doc comment after its first paragraph (the whole doc comment when `summary` is given) |
| `status` | Success-response status override (must be `2xx`), e.g. `status = 204`; re-keys the inferred `200` response (no body for `204`/`304`) |
| `error_status` | Extra error status codes to include in OpenAPI responses; declaring any makes the explicit error set authoritative (see below) |
| `responses` | Typed error responses, e.g. `responses = [(404, NotFoundError), (400, crate::errors::BadRequestError)]`; declaring any makes the explicit error set authoritative (see below) |
| `security` | Per-operation security requirements, e.g. `security = ["bearerAuth"]`; `security = []` emits explicit no auth |
| `headers` | Header parameters consumed by custom extractors, e.g. `headers = [{ name = "Authorization", required = true, description = "Bearer token" }]`; `required` defaults to `false` |
| `request_example` | Operation-level request body example as a JSON string; invalid JSON is emitted as a JSON string value |
| `response_example` | Operation-level `200` response example as a JSON string; invalid JSON is emitted as a JSON string value |
| `deprecated` | Bare flag marking the OpenAPI operation as deprecated |

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
| `spec_url` | `VESPERA_SPEC_URL` |
| `servers` | `VESPERA_SERVER_URL` + `VESPERA_SERVER_DESCRIPTION` |

**Priority**: Macro parameter > Environment variable > Default

Without `servers` the document has no `servers` entry, so clients use the origin that serves it. `info.description` defaults to the crate's `description` from `Cargo.toml`.

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

### Same-File Model Reference

When the model is in the same file, you can use a simple name with `name` parameter:

```rust
// In src/models/user.rs
pub struct Model {
    pub id: i32,
    pub name: String,
    pub email: String,
}

// Simple `Model` path works when using `name` parameter
vespera::schema_type!(Schema from Model, name = "UserSchema");
```

The macro infers the module path from the file location, so relation types like `HasOne<super::user::Entity>` are resolved correctly.

### Cross-File References

Reference structs from other files using full module paths:

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

### Partial Updates (PATCH)

Use `partial` to make fields optional for PATCH-style updates:

```rust
// All fields become Option<T>
schema_type!(UserPatch from User, partial);

// Only specific fields become Option<T>
schema_type!(UserPatch from User, partial = ["name", "email"]);
```

### Serde Rename All

Apply serde rename_all strategy:

```rust
// Convert field names to camelCase in JSON
schema_type!(UserDTO from User, rename_all = "camelCase");

// Available: "camelCase", "snake_case", "PascalCase", "SCREAMING_SNAKE_CASE", etc.
```

### Omit Fields with Database Defaults (`omit_default`)

Automatically omit fields that have database-level defaults — perfect for create DTOs where the database handles `id`, `created_at`, etc.:

```rust
#[derive(DeriveEntityModel)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]             // ← has default (auto-increment)
    pub id: i32,
    pub title: String,
    pub content: String,
    #[sea_orm(default_value = "NOW()")]  // ← has default (SQL function)
    pub created_at: DateTimeWithTimeZone,
}

// Omits `id` (primary_key) and `created_at` (default_value) automatically
schema_type!(CreatePostRequest from crate::models::post::Model, omit_default);
// Generated struct only has: title, content
```

`omit_default` detects fields with:
- `#[sea_orm(primary_key)]` — auto-increment / generated IDs
- `#[sea_orm(default_value = "...")]` — SQL defaults like `NOW()`, `gen_random_uuid()`, literals

Can be combined with other parameters:

```rust
// omit_default + add extra fields
schema_type!(CreateItemRequest from Model, omit_default, add = [("tags": Vec<String>)]);
```

### Database Defaults in OpenAPI

Fields with database defaults automatically get `default` values in the generated OpenAPI schema:

| SeaORM Attribute | OpenAPI Default |
|-----------------|-----------------|
| `primary_key` (Uuid) | `"00000000-0000-0000-0000-000000000000"` |
| `primary_key` (i32/i64) | `0` |
| `default_value = "NOW()"` | `"1970-01-01T00:00:00+00:00"` |
| `default_value = "gen_random_uuid()"` | `"00000000-0000-0000-0000-000000000000"` |
| `default_value = "true"` | `true` (literal passthrough) |

> **Note:** `required` is determined solely by nullability (`Option<T>`). Fields with defaults are still `required` unless they are `Option<T>`.

### SeaORM Integration

`schema_type!` has first-class support for SeaORM models with relations:

```rust
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, DeriveEntityModel)]
#[sea_orm(table_name = "memos")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    pub user: BelongsTo<super::user::Entity>,     // → Option<Box<UserSchema>>
    pub comments: HasMany<super::comment::Entity>, // → Vec<CommentSchema>
}

// Generates Schema with proper relation types
vespera::schema_type!(Schema from Model, name = "MemoSchema");
```

**Relation Type Conversions:**

| SeaORM Type | Generated Schema Type |
|-------------|----------------------|
| `HasOne<Entity>` | `Box<Schema>` or `Option<Box<Schema>>` |
| `BelongsTo<Entity>` | `Option<Box<Schema>>` |
| `HasMany<Entity>` | `Vec<Schema>` |
| `DateTimeWithTimeZone` | `chrono::DateTime<FixedOffset>` |

**Circular Reference Handling:** When schemas reference each other (e.g., User ↔ Memo), the macro automatically detects and handles circular references by inlining fields to prevent infinite recursion.

### Same-File Relation Adapters

For response DTOs that live in the same route file, `schema_type!` can now keep the handler code unchanged even when a SeaORM relation should be exposed through a custom local DTO.

Example:

```rust
#[derive(Serialize, vespera::Schema)]
#[serde(rename_all = "camelCase")]
pub struct UserInArticle {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub profile_image: Option<String>,
}

#[derive(Serialize, vespera::Schema)]
#[serde(rename_all = "camelCase")]
pub struct CategoryInArticle {
    pub id: i64,
    pub name: String,
    pub parent_category_id: Option<i64>,
    pub is_active: bool,
    pub is_menu: bool,
}

schema_type!(
    ArticleResponse from crate::models::article::Model,
    relation_adapters = [
        ("user", UserInArticle),
        ("category", CategoryInArticle),
    ],
    add = [("article_review_users": Vec<ArticleReviewUserInArticle>)]
);

// Existing handler code stays valid.
Ok(ArticleResponse {
    user: user.into(),
    category: category.into(),
    article_review_users,
    ..
})
```

How it works:

- `relation_adapters = [("field", AdapterStruct)]` is an explicit opt-in for single-value relation fields (`HasOne` / `BelongsTo`)
- The adapter struct name is used verbatim; Vespera does not infer adapter names by convention
- If an explicitly named adapter struct cannot be found in the same file, compilation fails instead of silently falling back
- Vespera generates local compile adapters so `Option<Model>.into()` works unchanged in the handler
- The internal `__Vespera…Relation` wrapper type stays private to Rust typing
- OpenAPI references the **adapter DTO's own schema** (`UserInArticle`, `CategoryInArticle`) — so the documented response shape matches exactly what the handler serializes, instead of over-promising the base relation schema (`UserSchema`, `CategorySchema`)

Use this when you want route-local response DTOs for single-value relations without rewriting the route construction logic. Relation fields not listed in `relation_adapters` keep the default base-schema relation type.

### Multipart Mode

Generate `Multipart` structs from existing types using the `multipart` keyword:

```rust
#[derive(vespera::Multipart, vespera::Schema)]
pub struct CreateUploadRequest {
    pub name: String,
    #[form_data(limit = "10MiB")]
    pub file: Option<FieldData<NamedTempFile>>,
    pub description: Option<String>,
}

// Generates a Multipart struct (no serde derives), all fields Optional
schema_type!(PatchUploadRequest from CreateUploadRequest, multipart, partial, omit = ["file"]);
```

When `multipart` is enabled:
- Derives `Multipart` instead of `Serialize`/`Deserialize`
- Suppresses `#[serde(...)]` attributes (multipart parsing is not serde-based)
- Preserves `#[form_data(...)]` attributes from source struct
- Skips SeaORM relation fields (nested objects can't be represented in multipart forms)
- Does not generate `From` impl

### Parameters

| Parameter | Description |
|-----------|-------------|
| `pick` | Include only specified fields |
| `omit` | Exclude specified fields |
| `rename` | Rename fields: `rename = [("old", "new")]` |
| `add` | Add new fields (disables auto `From` impl) |
| `clone` | Control Clone derive (default: true) |
| `partial` | Make fields optional: `partial` or `partial = ["field1"]` |
| `name` | Custom OpenAPI schema name: `name = "UserSchema"` |
| `rename_all` | Serde rename strategy: `rename_all = "camelCase"` |
| `ignore` | Skip Schema derive (bare keyword, no value) |
| `multipart` | Derive `Multipart` instead of serde (bare keyword) |
| `omit_default` | Auto-omit fields with DB defaults: `primary_key`, `default_value` (bare keyword) |

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

## Cron Jobs

Schedule background tasks with `#[vespera::cron]`. A tiny built-in tokio scheduler (one task per job, [croner](https://crates.io/crates/croner) for expression parsing) runs them.

### Enable Feature

```toml
[dependencies]
vespera = { version = "0.6", features = ["cron"] }
```

### Define Cron Jobs

Place `#[vespera::cron("...")]` on any `pub async fn` with zero parameters. The function can live anywhere in your project — no special directory required.

```rust
// src/cron/cleanup.rs, src/tasks.rs, or even src/routes/users.rs — anywhere works
#[vespera::cron("1/10 * * * * *")]
pub async fn cleanup_sessions() {
    println!("Running cleanup every 10 seconds");
}

#[vespera::cron("0 0 * * * *")]
pub async fn hourly_report() {
    println!("Running hourly report");
}
```

### How It Works

1. `#[cron("...")]` registers the job at compile time (like `#[route]`)
2. `vespera!()` auto-discovers all registered cron jobs — no extra parameters needed
3. A background scheduler spawns via `tokio::spawn` when the app starts (once
   per process, however often the router is built)

```rust
// No cron-specific config — just works
let app = vespera!(docs_url = "/docs");
```

### Cron Expression Format

Uses 6-field cron expressions (`sec min hour day month weekday`):

| Expression | Schedule |
|-----------|----------|
| `0 */5 * * * *` | Every 5 minutes |
| `0 0 * * * *` | Every hour |
| `0 0 0 * * *` | Daily at midnight |
| `1/10 * * * * *` | Every 10 seconds |
| `0 30 9 * * Mon-Fri` | Weekdays at 9:30 AM |

Expressions are validated at compile time by the very parser the runtime
scheduler uses ([croner](https://crates.io/crates/croner) 4: `L`, `W`, `#`,
`?`, names and an optional trailing year are supported), so an accepted
expression can never fail at startup. When both day-of-month and
day-of-week are restricted, **both** must match (AND, not the classic cron OR).

### Time Zones (default: UTC)

Expressions are evaluated in **UTC** unless the job names an IANA time zone
with `tz`. Time zones need the opt-in `cron-tz` feature (it adds
[chrono-tz](https://crates.io/crates/chrono-tz) and its embedded tz database,
~7 MB of generated source, which is why it is not part of `cron`):

```toml
vespera = { version = "0.6", features = ["cron", "cron-tz"] }
```

```rust
// 09:00 in Seoul every day (00:00 UTC)
#[vespera::cron("0 0 9 * * *", tz = "Asia/Seoul")]
pub async fn morning_report() {}
```

The name is checked at compile time (exact, case-sensitive IANA names such as
`"Asia/Seoul"`, `"America/New_York"`, `"UTC"`); using `tz` without the
`cron-tz` feature is a compile error. Daylight-saving transitions follow
croner: a fixed-time job whose time falls into a spring-forward gap runs right
after the gap, and runs once in a fall-back overlap.

### Overlapping Runs

By default every tick starts a run, so a job that takes longer than its
interval overlaps itself (`overlap = "allow"`). With `overlap = "skip"`, a tick
is skipped — and logged to stderr — while the previous run of the same job is
still going:

```rust
#[vespera::cron("0 */5 * * * *", overlap = "skip")]
pub async fn reindex() {}
```

### Panics and Shutdown

- Every run is supervised: a panicking job is logged to stderr
  (`vespera: cron job `module::function` panicked: …`) and its schedule keeps
  going.
- `vespera::cron::shutdown()` stops scheduling new runs (process-wide, for
  good); `vespera::cron::shutdown_and_wait(timeout).await` additionally waits
  up to `timeout` for in-flight runs and returns whether they all finished.
- `serve_with_graceful_shutdown(addr, signal)` does this for you: no new runs
  start once `signal` resolves, and after the server drained, in-flight runs
  get up to `vespera::cron::DEFAULT_SHUTDOWN_GRACE` (10 s).
- In a JNI library the jobs run on the shared JNI runtime and stop (in-flight
  runs are cancelled) with `VesperaBridge.shutdown(...)` / `shutdown0`.

### Requirements

- Functions must be `pub async fn`
- Functions must take **no parameters** (no `State`, no extractors)
- The `cron` feature must be enabled (`cron-tz` too for `tz = "..."`)

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

Relative paths resolve against the crate's `CARGO_MANIFEST_DIR` (the directory holding its `Cargo.toml`), so workspace members write next to their own manifest. vespera's build cache lives under the cargo target directory (`CARGO_TARGET_DIR` is honoured).

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
| `BTreeSet<T>`, `HashSet<T>` | `array` with `uniqueItems: true` |
| `Uuid` | `string` with `format: uuid` |
| `Decimal` | `string` with `format: decimal` |
| `NaiveDate` | `string` with `format: date` |
| `NaiveTime` | `string` with `format: time` |
| `DateTime`, `DateTimeWithTimeZone` | `string` with `format: date-time` |
| `FieldData<NamedTempFile>` | `string` with `format: binary` |
| `Ipv4Addr` / `Ipv6Addr` | `string` with `format: ipv4` / `ipv6` |
| `IpAddr`, `SocketAddr` (`V4` / `V6`), `PathBuf` | `string` |
| `url::Url` | `string` with `format: uri` |
| `serde_json::Value` | `{}` (any JSON value) |
| `std::time::Duration` (bare `Duration`, `std::time::` / `core::time::`) | `object` `{secs, nanos}` (serde's std shape) |
| other qualified `Duration` (`time::`, `chrono::`) | `{}` — no single serde shape; annotate explicitly |
| Custom struct | `$ref` to components/schemas |

Request / response bodies follow what axum actually sends:

| Handler type | OpenAPI |
|--------------|---------|
| `Path<Struct>` | one path parameter per `{name}`, typed by the field with that serde name |
| `Query<Struct>` | one query parameter per field; `#[serde(skip)]` / `skip_deserializing` fields are omitted, `#[serde(flatten)]` structs are expanded; `Vec<T>` fields use `style: form, explode: true` (repeated keys need `axum_extra::extract::Query`) |
| `Option<Json<T>>` | `requestBody.required: false` |
| `Bytes` / `BytesMut` / `Body` argument | `application/octet-stream` binary request body |
| `impl IntoResponse`, `Response`, `Redirect` | description-only response (no `content`) |
| `Html<T>` | `text/html` string |
| `Bytes`, `Vec<u8>`, `&[u8]` return | `application/octet-stream` binary |
| `String` / `&str` return | `text/plain` |

Serde container attributes are honoured: enum `rename_all` renames variants and
`rename_all_fields` renames struct-variant fields, `deny_unknown_fields` emits
`additionalProperties: false` (unless the struct flattens), and a
`#[serde(flatten)] Option<T>` contributes `T`'s fields as optional.

---

## JNI / Java Integration

Embed your Vespera router inside a Java/Spring application — no TCP, no JSON envelope overhead.

```rust
// Cargo.toml
// vespera = { version = "0.6", features = ["jni"] }

pub fn create_app() -> axum::Router {
    vespera!(title = "My API")
}

vespera::jni_app!(create_app);
```

```java
@SpringBootApplication
public class MyApp {
    public static void main(String[] args) {
        VesperaBridge.init("my_rust_lib");
        SpringApplication.run(MyApp.class, args);
    }
}
```

> Do **not** add `com.devfive.vespera.bridge` to `@ComponentScan` — the bridge is
> registered by Spring Boot autoconfiguration. Scanning it picks the controller
> up as a plain `@RestController`, which fails to construct and bypasses
> `vespera.bridge.controller-enabled`.

The `VesperaProxyController` auto-registers as a catch-all and forwards every HTTP request through a length-prefixed **binary wire format** (`[u32 BE | UTF-8 JSON header | raw body]`) — multipart uploads, PDFs, and images travel raw, with zero base64 overhead.

See [`libs/vespera-bridge`](./libs/vespera-bridge/) for the Java library docs and [`examples/rust-jni-demo`](./examples/rust-jni-demo/) for a complete end-to-end demo.

## Project Structure

```
vespera/
├── crates/
│   ├── vespera/              # Main crate - re-exports everything
│   ├── vespera_core/         # OpenAPI types and abstractions
│   ├── vespera_macro/        # Proc-macros (compile-time magic)
│   ├── vespera_inprocess/    # In-process axum dispatch + binary wire API
│   └── vespera_jni/          # JNI glue (Runtime + JNI symbol)
├── libs/
│   ├── vespera-bridge/       # Java library (kr.devfive:vespera-bridge)
│   └── vespera-bridge-gradle-plugin/  # Gradle plugin (kr.devfive.vespera-bridge)
├── examples/
│   ├── axum-example/         # Standalone OpenAPI server
│   ├── rust-jni-demo/        # Rust + Spring Boot JNI integration
│   └── testkit/              # In-process test client used by the examples
└── benches/                  # Compile-time macro benchmarks (not default members)
```

---

## Contributing

```bash
git clone https://github.com/dev-five-git/vespera.git
cd vespera

# Build & test
cargo build
cargo test --workspace

# Lint gates (also enforced in CI: fmt, clippy, oxlint)
bun install --frozen-lockfile
bun run lint

# Supply-chain check (cargo install cargo-deny --locked)
cargo deny check

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
