# PARSER MODULE

Extracts types from Rust AST to build OpenAPI schemas/parameters.

## STRUCTURE

A `.rs` file followed by `+ dir/` is a module file with a sibling directory of
child modules; a bare `dir/` is a directory module with its own `mod.rs`.

```
parser/
├── mod.rs                          # Re-exports public API
├── schema/                         # Rust types → JSON Schema (see below)
├── parameters.rs + parameters/     # Handler params → OpenAPI Parameters
│                                   #   parameters/{header,path,query,shared}.rs
├── operation.rs + operation/       # Function → OpenAPI Operation (operation/tests.rs)
├── request_body.rs                 # Json<T> → requestBody
├── response.rs + response/         # Return type → response schema (response/tests.rs)
├── path.rs                         # Path parameter extraction
├── extractors.rs                   # Validated<T> unwrapping helpers
├── extractor_validation.rs         # Compile-time Schema-backed extractor check
├── is_keyword_type.rs              # Axum extractor detection
└── snapshots/                      # insta test snapshots
```

```
parser/schema/
├── mod.rs                          # Re-exports the schema public API
├── type_schema.rs + type_schema/   # Type → SchemaRef (type_schema/conversion.rs)
├── struct_schema.rs + struct_schema/ # Struct → JSON Schema (struct_schema/tests.rs)
├── enum_schema.rs + enum_schema/   # Enum → JSON Schema
│                                   #   enum_schema/{representations.rs,unit.rs,variant.rs}
├── serde_attrs.rs + serde_attrs/   # Serde attribute extraction
│                                   #   serde_attrs/{common,enum_repr,extract,fallback,rename_case}.rs
├── generics.rs                     # Generic type parameter substitution
├── schema_attrs.rs                 # #[schema(...)] constraint extraction
└── snapshots/                      # insta test snapshots
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| Add schema type support | `schema/type_schema/conversion.rs` | `parse_type_to_schema_ref()` |
| Add struct/enum schema shape | `schema/struct_schema.rs`, `schema/enum_schema.rs` | Re-exported via `schema/mod.rs` |
| Change serde attribute handling | `schema/serde_attrs/` | `extract.rs`, `enum_repr.rs`, `rename_case.rs` |
| Handle new extractor | `parameters.rs` | `parse_function_parameter()`, dispatching into `parameters/` |
| Unwrap `Validated<T>` | `extractors.rs` | `unwrap_validated_type()` |
| Reject non-`Schema` extractor types | `extractor_validation.rs` | Compile-time diagnostic |
| Modify operation generation | `operation.rs` | `build_operation_from_function()` |
| Add response type | `response.rs` | `parse_return_type()` |
| Detect Axum types | `is_keyword_type.rs` | Keyword matching |

## KEY FUNCTIONS

`parse_struct_to_schema`, `parse_enum_to_schema` and the `extract_*` serde
helpers are re-exported from `parser/mod.rs`, so import them from `parser` —
not from their defining submodule.

| Function | File | Purpose |
|----------|------|---------|
| `parse_struct_to_schema()` | `schema/struct_schema.rs` | Struct → JSON Schema object |
| `parse_enum_to_schema()` | `schema/enum_schema.rs` | Enum → oneOf/enum schema |
| `parse_type_to_schema_ref()` | `schema/type_schema/conversion.rs` | Type → `SchemaRef` (main entry) |
| `extract_rename_all()` | `schema/serde_attrs/extract.rs` | Serde attribute parsing |
| `parse_function_parameter()` | `parameters.rs` | FnArg → Parameter[] |
| `parse_request_body()` | `request_body.rs` | FnArg → requestBody |
| `parse_return_type()` | `response.rs` | ReturnType → responses |
| `build_operation_from_function()` | `operation.rs` | ItemFn → Operation |
| `validate_schema_backed_extractors_with_cache()` | `extractor_validation.rs` | Extractor generic must derive `Schema` |

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
