# PARSER MODULE

Extracts types from Rust AST to build OpenAPI schemas/parameters.

## STRUCTURE

```
parser/
├── mod.rs              # Re-exports public API
├── schema.rs           # Rust types → JSON Schema (1527 lines)
├── parameters.rs       # Handler params → OpenAPI Parameters (845 lines)
├── operation.rs        # Function → OpenAPI Operation
├── request_body.rs     # Json<T> → requestBody
├── response.rs         # Return type → response schema
├── path.rs             # Path parameter extraction
├── is_keyword_type.rs  # Axum extractor detection
└── snapshots/          # insta test snapshots
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| Add schema type support | `schema.rs` | `type_to_schema_type()` |
| Handle new extractor | `parameters.rs` | `parse_function_parameter()` |
| Modify operation generation | `operation.rs` | `build_operation_from_function()` |
| Add response type | `response.rs` | `extract_response_schema()` |
| Detect Axum types | `is_keyword_type.rs` | Keyword matching |

## KEY FUNCTIONS

| Function | File | Purpose |
|----------|------|---------|
| `parse_struct_to_schema()` | schema.rs | Struct → JSON Schema object |
| `parse_enum_to_schema()` | schema.rs | Enum → oneOf/enum schema |
| `parse_function_parameter()` | parameters.rs | FnArg → Parameter[] |
| `build_operation_from_function()` | operation.rs | ItemFn → Operation |
| `extract_rename_all()` | schema.rs | Serde attribute parsing |

## CONVENTIONS

- **BTreeMap**: Always use for deterministic output
- **SchemaRef**: Inline or $ref - prefer $ref for complex types
- **known_schemas**: Pass around to resolve cross-references

## TYPE MAPPING

| Rust Type | OpenAPI Schema |
|-----------|----------------|
| `String`, `&str` | `{ type: "string" }` |
| `i32`, `u32`, etc | `{ type: "integer" }` |
| `f32`, `f64` | `{ type: "number" }` |
| `bool` | `{ type: "boolean" }` |
| `Vec<T>` | `{ type: "array", items: T }` |
| `Option<T>` | T schema (nullable in parent) |
| `HashMap<K,V>` | `{ type: "object", additionalProperties: V }` |
| Custom struct | `{ $ref: "#/components/schemas/Name" }` |

## EXTRACTOR HANDLING

| Axum Extractor | OpenAPI Location |
|----------------|------------------|
| `Path<T>` | path parameter |
| `Query<T>` | query parameters |
| `Json<T>` | requestBody |
| `State<T>` | ignored |
| `TypedHeader<T>` | header parameter |

## ANTI-PATTERNS

- **NEVER** hardcode schema names - use `known_schemas` lookup
- **NEVER** panic on unknown types - return sensible default
- Serde attributes **MUST** be respected (rename, rename_all, default)
