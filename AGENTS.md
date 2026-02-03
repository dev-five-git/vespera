# VESPERA PROJECT KNOWLEDGE BASE

**Generated:** 2026-02-04
**Branch:** main

## OVERVIEW

Vespera is a fully automated OpenAPI 3.1 engine for Axum - delivers FastAPI-like DX to Rust. Zero-config route discovery via compile-time macro scanning.

## STRUCTURE

```
vespera/
├── crates/
│   ├── vespera/           # Public API - re-exports everything (+ chrono re-export)
│   ├── vespera_core/      # OpenAPI types, route/schema abstractions
│   └── vespera_macro/     # Proc-macros (main logic lives here)
└── examples/axum-example/ # Demo app with route patterns
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Add new macro feature | `crates/vespera_macro/src/` | Main macro in `lib.rs` |
| Modify OpenAPI output | `crates/vespera_macro/src/openapi_generator.rs` | JSON generation |
| Add route parser feature | `crates/vespera_macro/src/parser/` | Type extraction logic |
| Change schema generation | `crates/vespera_macro/src/parser/schema.rs` | Rust→JSON Schema |
| Modify route attribute | `crates/vespera_macro/src/args.rs` | `#[route]` parsing |
| Modify schema_type! macro | `crates/vespera_macro/src/schema_macro.rs` | Type derivation & SeaORM support |
| Add core types | `crates/vespera_core/src/` | OpenAPI spec types |
| Test new features | `examples/axum-example/` | Add route, run example |

## KEY COMPONENTS

| File | Lines | Role |
|------|-------|------|
| `vespera_macro/src/lib.rs` | ~1044 | `vespera!`, `#[route]`, `#[derive(Schema)]` |
| `vespera_macro/src/schema_macro.rs` | ~3000 | `schema_type!` macro, SeaORM relation handling |
| `vespera_macro/src/parser/schema.rs` | ~1527 | Rust struct → JSON Schema conversion |
| `vespera_macro/src/parser/parameters.rs` | ~845 | Extract path/query params from handlers |
| `vespera_macro/src/openapi_generator.rs` | ~808 | OpenAPI doc assembly |
| `vespera_macro/src/collector.rs` | ~707 | Filesystem route scanning |

## SCHEMA_TYPE! MACRO

Generate request/response types from existing structs with powerful transformations.

### Key Features
- **Same-file Model reference**: `schema_type!(Schema from Model, name = "UserSchema")` - infers module path from file location
- **Cross-file reference**: `schema_type!(Response from crate::models::user::Model, omit = ["password"])`
- **SeaORM integration**: Automatic conversion of `HasOne`, `BelongsTo`, `HasMany` relations
- **Chrono conversion**: `DateTimeWithTimeZone` → `vespera::chrono::DateTime<FixedOffset>`
- **Circular reference handling**: Automatic detection and inline field generation

### Parameters
| Parameter | Description |
|-----------|-------------|
| `pick` | Include only specified fields |
| `omit` | Exclude specified fields |
| `rename` | Rename fields: `[("old", "new")]` |
| `add` | Add new fields (disables auto `From`) |
| `clone` | Control Clone derive (default: true) |
| `partial` | Make fields optional for PATCH |
| `name` | Custom OpenAPI schema name |
| `rename_all` | Serde rename strategy |
| `ignore` | Skip Schema derive |

### Module Path Resolution
When using simple `Model` path (no `crate::` prefix):
1. `find_struct_from_path()` calls `find_struct_by_name_in_all_files()`
2. Uses `schema_name` hint to disambiguate (e.g., "UserSchema" → prefers `user.rs`)
3. `file_path_to_module_path()` infers module path from file location
4. This enables `super::` resolution in relation types

## CONVENTIONS

- **Rust 2024 edition** across all crates
- **Workspace dependencies**: Internal crates use `{ workspace = true }`
- **Version sync**: All crates at 0.1.19
- **Test frameworks**: `rstest` for unit tests, `insta` for snapshots
- **No `build.rs`**: All code gen via proc-macros at compile time

## ANTI-PATTERNS (THIS PROJECT)

- **NEVER** add `build.rs` - macro handles compile-time generation
- **NEVER** manually register routes - `vespera!` macro discovers them
- **NEVER** write OpenAPI JSON by hand - generated from code
- Route functions **MUST** be `pub async fn`

## ARCHITECTURE FLOW

```
User writes:           vespera!() macro at compile-time:
┌──────────────┐      ┌────────────────────────────────────────┐
│ src/routes/  │ ──── │ 1. Scan filesystem for .rs files       │
│   users.rs   │      │ 2. Parse #[route] attributes           │
│   posts.rs   │      │ 3. Extract handler signatures          │
└──────────────┘      │ 4. Generate Axum Router code           │
                      │ 5. Build OpenAPI spec                   │
                      │ 6. Write openapi.json (optional)       │
                      │ 7. Inject Swagger/ReDoc routes         │
                      └────────────────────────────────────────┘
```

## COMMANDS

```bash
# Development
cargo build                    # Build all crates
cargo test --workspace         # Run all tests
cargo test -p vespera_macro    # Test macros only

# Run example
cd examples/axum-example
cargo run                      # Starts server on :3000
# Visit http://localhost:3000/docs for Swagger UI

# Check generated OpenAPI
cat examples/axum-example/openapi.json
```

## NOTES

- Macro performs **filesystem I/O at compile time** - may affect IDE performance
- OpenAPI files are **regenerated on every build** when `openapi = "..."` specified
- `CARGO_MANIFEST_DIR` env var used to locate `src/routes/` folder
- Generic types in schemas require `#[derive(Schema)]` on all type params
