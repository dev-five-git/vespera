# VESPERA PROJECT KNOWLEDGE BASE

**Generated:** 2026-01-07
**Commit:** 939a801
**Branch:** main

## OVERVIEW

Vespera is a fully automated OpenAPI 3.1 engine for Axum - delivers FastAPI-like DX to Rust. Zero-config route discovery via compile-time macro scanning.

## STRUCTURE

```
vespera/
├── crates/
│   ├── vespera/           # Public API - re-exports everything
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
| Add core types | `crates/vespera_core/src/` | OpenAPI spec types |
| Test new features | `examples/axum-example/` | Add route, run example |

## KEY COMPONENTS

| File | Lines | Role |
|------|-------|------|
| `vespera_macro/src/lib.rs` | 1044 | `vespera!`, `#[route]`, `#[derive(Schema)]` |
| `vespera_macro/src/parser/schema.rs` | 1527 | Rust struct → JSON Schema conversion |
| `vespera_macro/src/parser/parameters.rs` | 845 | Extract path/query params from handlers |
| `vespera_macro/src/openapi_generator.rs` | 808 | OpenAPI doc assembly |
| `vespera_macro/src/collector.rs` | 707 | Filesystem route scanning |

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
