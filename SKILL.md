---
name: vespera-development
description: Develop and extend the Vespera OpenAPI engine for Axum. Covers both library development and usage patterns.
---

# Vespera Development Guide

Vespera = FastAPI DX for Rust. Zero-config OpenAPI 3.1 generation via compile-time macro scanning.

## Quick Reference

### For Users (Building APIs with Vespera)

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

### For Contributors (Extending Vespera)

| Want to... | Look at | Key function |
|------------|---------|--------------|
| Add macro parameter | `vespera_macro/src/lib.rs` | `AutoRouterInput::parse()` |
| Support new Axum extractor | `vespera_macro/src/parser/parameters.rs` | `parse_function_parameter()` |
| Add Rust→JSON Schema type | `vespera_macro/src/parser/schema.rs` | `type_to_schema_type()` |
| Change OpenAPI output | `vespera_macro/src/openapi_generator.rs` | `generate_openapi_doc_with_metadata()` |
| Modify route scanning | `vespera_macro/src/collector.rs` | `collect_metadata()` |

---

## Architecture

```
Compile Time (vespera! macro):
┌─────────────────────────────────────────────────────────────────┐
│ 1. Scan src/routes/ for .rs files (collector.rs)                │
│ 2. Parse #[route] attributes (args.rs, route/)                  │
│ 3. Extract handler signatures (parser/parameters.rs)            │
│ 4. Convert Rust types → JSON Schema (parser/schema.rs)          │
│ 5. Build OpenAPI document (openapi_generator.rs)                │
│ 6. Write openapi.json to disk (if configured)                   │
│ 7. Generate Axum Router TokenStream (lib.rs)                    │
│ 8. Inject Swagger/ReDoc HTML routes (lib.rs)                    │
└─────────────────────────────────────────────────────────────────┘

Runtime:
┌─────────────────────────────────────────────────────────────────┐
│ Standard Axum server - nothing special                          │
└─────────────────────────────────────────────────────────────────┘
```

---

## Common Tasks

### Adding a New Macro Parameter

```rust
// 1. Add field to AutoRouterInput (lib.rs:97)
struct AutoRouterInput {
    // ...existing fields...
    my_new_param: Option<LitStr>,  // add here
}

// 2. Parse it in Parse impl (lib.rs:107)
impl Parse for AutoRouterInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // add case in match:
        "my_new_param" => {
            input.parse::<syn::Token![=]>()?;
            my_new_param = Some(input.parse()?);
        }
    }
}

// 3. Use in vespera() function (lib.rs:379)
let my_new_param = input.my_new_param.map(|p| p.value());
```

### Supporting a New Axum Extractor

```rust
// In parser/parameters.rs, add to parse_function_parameter()

// Example: Support Extension<T>
if ident_str == "Extension" {
    // Extract inner type T
    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
        // ... parse inner type
        // Return None to ignore, or Some(vec![Parameter {...}]) to document
    }
    return None;  // Extensions are typically internal, ignore
}
```

### Adding a New Rust Type → JSON Schema Mapping

```rust
// In parser/schema.rs, find type_to_schema_type() or parse_type_to_schema_ref()

// Example: Support chrono::DateTime<Utc>
if type_name == "DateTime" {
    return SchemaRef::Inline(Schema {
        schema_type: Some(SchemaType::String),
        format: Some("date-time".to_string()),
        ..Default::default()
    });
}
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

## Testing

```bash
# Run all tests
cargo test --workspace

# Test macros only (most important)
cargo test -p vespera_macro

# Update snapshots (after intentional changes)
cargo insta review

# Run example to verify end-to-end
cd examples/axum-example && cargo run
# → http://localhost:3000/docs
```

### Writing Tests for Parser Changes

```rust
// Use rstest for parameterized tests
#[rstest]
#[case::string("String", SchemaType::String)]
#[case::integer("i32", SchemaType::Integer)]
fn test_type_mapping(#[case] rust_type: &str, #[case] expected: SchemaType) {
    let schema = parse_type_to_schema(rust_type);
    assert_eq!(schema.schema_type, Some(expected));
}

// Use insta for snapshot tests (complex outputs)
#[test]
fn test_openapi_generation() {
    let openapi = generate_openapi_doc(...);
    insta::assert_json_snapshot!(openapi);
}
```

---

## Anti-Patterns

### NEVER Do

```rust
// ❌ Use HashMap for OpenAPI output (non-deterministic order)
let paths: HashMap<String, PathItem> = ...;

// ✅ Always BTreeMap for deterministic JSON
let paths: BTreeMap<String, PathItem> = ...;
```

```rust
// ❌ Unwrap without context in collector
let content = std::fs::read_to_string(&file).unwrap();

// ✅ Use anyhow with context
let content = std::fs::read_to_string(&file)
    .with_context(|| format!("Failed to read: {}", file.display()))?;
```

```rust
// ❌ Panic on unknown types
panic!("Unknown type: {}", type_name);

// ✅ Return sensible default
Schema {
    schema_type: Some(SchemaType::Object),
    description: Some(format!("Unknown type: {}", type_name)),
    ..Default::default()
}
```

```rust
// ❌ Add build.rs for code generation
// build.rs - DON'T DO THIS

// ✅ Everything via proc-macro at compile time
// vespera! macro handles all generation
```

### Route Handler Requirements

```rust
// ❌ Private function
async fn get_users() -> Json<Vec<User>> { ... }

// ❌ Non-async function
pub fn get_users() -> Json<Vec<User>> { ... }

// ✅ Must be pub async fn
pub async fn get_users() -> Json<Vec<User>> { ... }
```

---

## Debugging Tips

### Macro Expansion

```bash
# See what vespera! generates
cargo expand --package axum-example

# Or specific function
cargo expand --package axum-example main
```

### OpenAPI Output Issues

```bash
# Check generated JSON
cat examples/axum-example/openapi.json | jq .

# Validate against OpenAPI spec
npx @apidevtools/swagger-cli validate openapi.json
```

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
