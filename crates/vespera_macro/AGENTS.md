# VESPERA_MACRO CRATE

Proc-macro crate - all compile-time magic happens here.

## STRUCTURE

```
vespera_macro/
├── src/
│   ├── lib.rs              # Macro entry points: vespera!, #[route], #[derive(Schema)]
│   ├── args.rs             # #[route] attribute argument parsing
│   ├── collector.rs        # Filesystem scanning, route discovery
│   ├── file_utils.rs       # Path manipulation utilities
│   ├── metadata.rs         # RouteMetadata, StructMetadata types
│   ├── method.rs           # HTTP method enum handling
│   ├── openapi_generator.rs # OpenAPI JSON assembly
│   ├── parser/             # Type extraction (see parser/AGENTS.md)
│   └── route/              # Route info extraction
└── tests/                  # Unit tests with tempfile
```

## WHERE TO LOOK

| Task | File | Function/Section |
|------|------|------------------|
| Add macro parameter | `lib.rs` | `AutoRouterInput`, `parse()` impl |
| Add HTTP method | `method.rs` | `http_method_to_token_stream` |
| Change route scanning | `collector.rs` | `collect_metadata()` |
| Modify OpenAPI output | `openapi_generator.rs` | `generate_openapi_doc_with_metadata()` |
| Change #[route] attrs | `args.rs` | `RouteArgs` struct |

## KEY FUNCTIONS

| Function | Location | Purpose |
|----------|----------|---------|
| `vespera()` | lib.rs:379 | Main macro entry - orchestrates everything |
| `route()` | lib.rs:28 | Attribute macro - validates handler functions |
| `derive_schema()` | lib.rs:67 | Derive macro for Schema trait |
| `collect_metadata()` | collector.rs:11 | Scans folder, extracts route/struct info |
| `generate_router_code()` | lib.rs:496 | Generates Axum Router TokenStream |

## CONVENTIONS

- **syn/quote**: Standard proc-macro tooling
- **anyhow**: Error handling in collector
- **BTreeMap**: Ordered output for deterministic OpenAPI
- **SCHEMA_STORAGE**: Static mutex for cross-macro state

## ANTI-PATTERNS

- **NEVER** use HashMap for OpenAPI output (non-deterministic order)
- **NEVER** unwrap without context in collector (use anyhow)
- Route functions **MUST** be validated: `pub` + `async`

## TESTING

```bash
cargo test -p vespera_macro

# Snapshot tests use insta
cargo insta review
```

## GOTCHAS

- `CARGO_MANIFEST_DIR` is user's project, not this crate
- Schema storage is process-global via LazyLock<Mutex<>>
- Filesystem I/O happens at compile time (can affect IDE)
