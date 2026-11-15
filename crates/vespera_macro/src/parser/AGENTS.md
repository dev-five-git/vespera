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
│                                   #   + parameters/tests_shapes.rs
├── operation.rs + operation/       # Function → OpenAPI Operation
│                                   #   operation/{tests,tests_extractor_wrappers,tests_form_query,
│                                   #   tests_request_shapes,tests_status,tests_validated}.rs
├── request_body.rs                 # Json<T> / Form<T> / TypedMultipart<T> / raw bodies → requestBody
├── response.rs + response/         # Return type → responses (response/{tests,tests_parts}.rs)
├── result_alias.rs + result_alias/ # Return-type alias expansion (type ApiResult<T> = ..)
├── path.rs                         # Path parameter extraction
├── extractors.rs                   # Validated<T> / Result / WithRejection / Cached unwrapping
├── extractor_validation.rs + extractor_validation/ # Compile-time Schema-backed extractor check
│                                   #   (extractor_validation/tests_scope.rs)
├── is_keyword_type.rs              # Response keyword types (HeaderMap / StatusCode / CookieJar / Result; used by response.rs)
└── snapshots/                      # insta test snapshots
```

```
parser/schema/
├── mod.rs                          # Re-exports the schema public API
├── type_schema.rs + type_schema/   # Type → SchemaRef
│                                   #   type_schema/{conversion.rs + conversion/{schema_def,tests,
│                                   #   tests_instances}.rs, generic_instances.rs, nullable.rs}
├── struct_schema.rs + struct_schema/ # Struct → JSON Schema
│                                   #   struct_schema/{flatten,tests,tests_serde_fidelity,
│                                   #   tests_transparent,tests_wire_names,tests_wire_override}.rs
├── enum_schema.rs + enum_schema/   # Enum → JSON Schema
│                                   #   enum_schema/{representations.rs + representations/
│                                   #   {tests,tests_closed,tests_fidelity}.rs, unit.rs, variant.rs,
│                                   #   tests_validate.rs}
├── serde_attrs.rs + serde_attrs/   # Serde attribute extraction
│                                   #   serde_attrs/{codec,common,enum_repr,extract,fallback,rename_case}.rs
├── garde_rules.rs + garde_rules/   # #[garde(..)] rules → JSON Schema keywords (garde_rules/tests.rs)
├── generics.rs                     # Generic type parameter substitution
├── schema_attrs.rs                 # #[schema(...)] constraint extraction (incl. value_type)
├── scoped_ident.rs                 # Module-scoped keys for renames sharing an ident
├── wire_override.rs                # value_type / serde with / into / from schema overrides
├── request_view.rs + request_view/ # {Name}Input request views (request_view/tests.rs)
└── snapshots/                      # insta test snapshots
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| Add schema type support | `schema/type_schema/conversion.rs` | `parse_type_to_schema_ref()` |
| Add struct/enum schema shape | `schema/struct_schema.rs`, `schema/enum_schema.rs` | Re-exported via `schema/mod.rs` |
| Change serde attribute handling | `schema/serde_attrs/` | `extract.rs`, `enum_repr.rs`, `rename_case.rs` |
| Handle new extractor | `parameters.rs` | `parse_function_parameter()`, dispatching into `parameters/` |
| Unwrap `Validated<T>` / `Result<T, Rejection>` / `WithRejection<T, R>` / `Cached<T>` | `extractors.rs` | `request_extractor()` |
| Reject non-`Schema` extractor types | `extractor_validation.rs` | Compile-time diagnostic |
| Modify operation generation | `operation.rs` | `build_operation_from_function()` |
| Add response type | `response.rs` | `parse_return_type()` |
| Detect response keyword types (`HeaderMap` / `StatusCode` / `CookieJar` / `Result`) | `is_keyword_type.rs` | Last-segment match, used by `response.rs` (extractors: `extractors.rs` / `parameters.rs`) |

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
| `Option<T>` | T schema made nullable: `type: [T, "null"]`; a `$ref` (or a composition) becomes `anyOf: [<schema>, { type: "null" }]` |
| `HashMap<K,V>` | `{ type: "object", additionalProperties: V }` |
| Custom struct | `{ $ref: "#/components/schemas/Name" }` |

## EXTRACTOR HANDLING

| Axum Extractor | OpenAPI Location |
|----------------|------------------|
| `Path<T>` / `Option<Path<T>>` | path parameters (one per `{name}` of the route path) |
| `Query<T>` / `OptionalQuery<T>` | query parameters (`OptionalQuery`: every parameter optional) |
| `Form<T>` | requestBody `application/x-www-form-urlencoded` (query parameters with `form_in_query`, e.g. GET) |
| `Json<T>` | requestBody `application/json` |
| `TypedMultipart<T>` / `TypedMultipartWithLimits<T, ..>` / `Multipart` | requestBody `multipart/form-data` |
| `Validated<E>` / `Result<E, R>` / `WithRejection<E, R>` / `Cached<E>` | documented as the wrapped extractor `E` |
| `State<T>` | ignored |
| `TypedHeader<T>` | header parameter (reserved names `Accept` / `Content-Type` / `Authorization` are dropped; route-level `headers = [...]` keeps them as written) |

## ANTI-PATTERNS

- **NEVER** hardcode schema names - use `known_schemas` lookup
- **NEVER** panic on unknown types - return sensible default
- Serde attributes **MUST** be respected (rename, rename_all, default)
