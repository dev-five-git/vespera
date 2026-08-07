# VESPERA_MACRO CRATE

Proc-macro crate - all compile-time magic happens here.

## STRUCTURE

Every `mod` declared in `src/lib.rs` (lines 44-64) appears below. A `.rs` file
followed by `+ dir/` is a module file with a sibling directory of child modules;
a bare `dir/` is a directory module with its own `mod.rs`.

```
vespera_macro/
├── src/
│   ├── lib.rs                    # Proc-macro entry points only (thin dispatch to *_impl)
│   ├── args.rs                   # #[route] attribute argument parsing (RouteArgs)
│   ├── collector.rs + collector/ # Filesystem scanning, route discovery
│   ├── cron_impl.rs              # #[cron("...")] attribute macro implementation
│   ├── error.rs                  # MacroResult<T> = Result<T, syn::Error>, err_call_site
│   ├── file_utils.rs             # Path manipulation utilities
│   ├── garde_emit.rs + garde_emit/ # Emit garde::Validate impls (feature = "validation")
│   ├── http.rs                   # HTTP method constants and validation
│   ├── macro_storage.rs          # Per-crate storage for #[route] / #[cron] metadata
│   ├── metadata.rs               # RouteMetadata, StructMetadata types
│   ├── method.rs                 # HTTP method token stream generation
│   ├── multipart_impl/           # #[derive(Multipart)] implementation
│   ├── openapi_generator.rs + openapi_generator/ # OpenAPI JSON assembly
│   ├── parser/                   # Type extraction (see parser/AGENTS.md)
│   ├── route/                    # Route info extraction
│   ├── route_impl.rs             # #[route] attribute macro implementation
│   ├── router_codegen.rs + router_codegen/ # Macro input parsing + Router codegen
│   ├── schema_assertions.rs      # Marker impl + compile-time asserts from #[derive(Schema)]
│   ├── schema_impl.rs + schema_impl/ # #[derive(Schema)] implementation
│   ├── schema_macro/             # schema!/schema_type! macros, SeaORM, file_cache
│   └── vespera_impl.rs + vespera_impl/ # vespera!/export_app! orchestration
└── tests/                        # trybuild UI diagnostics (tests/ui/*.rs + .stderr)
```

Unit tests live **inline** (`#[cfg(test)] mod tests`) next to the code they
cover, per the repo-wide convention; `tests/` holds only the trybuild
compile-failure suite.

## WHERE TO LOOK

| Task | File | Function/Section |
|------|------|------------------|
| Add macro parameter | `router_codegen/input.rs` | `AutoRouterInput`, `parse()` impl |
| Add HTTP method | `method.rs` | `http_method_to_token_stream` |
| Change route scanning | `collector.rs` | `collect_metadata_from_files()` (`collect_metadata()` is `#[cfg(test)]`-only) |
| Modify OpenAPI output | `openapi_generator.rs` | `generate_openapi_doc_with_metadata()` |
| Change #[route] attrs | `args.rs` | `RouteArgs` struct |

## KEY FUNCTIONS

| Function | Location | Purpose |
|----------|----------|---------|
| `vespera()` | lib.rs:332 | Main macro entry - orchestrates everything |
| `route()` | lib.rs:76 | Attribute macro - validates handler functions |
| `derive_schema()` | lib.rs:122 | Derive macro for Schema trait |
| `collect_metadata_from_files()` | collector.rs:122 | Scans collected files, extracts route/struct info |
| `generate_router_code()` | router_codegen/generator.rs:111 | Generates Axum Router TokenStream |

## CONVENTIONS

- **syn/quote**: Standard proc-macro tooling
- **`syn::Error`**: The only error type; this crate has no error-handling
  dependency at all. Fallible code returns `crate::error::MacroResult<T>`
  (= `Result<T, syn::Error>`, `error.rs:34`) and builds errors with
  `err_call_site(...)` (`error.rs:38`) for call-site diagnostics or
  `syn::Error::new_spanned(node, msg)` / `syn::Error::new(span, msg)` when a
  specific AST node should be underlined. Entry points in `lib.rs` convert the
  `Err` into `e.to_compile_error()`, so failures are spanned `compile_error!`s
  instead of macro panics.
- **BTreeMap**: Ordered output for deterministic OpenAPI
- **SCHEMA_STORAGE**: Static mutex for cross-macro state

## ANTI-PATTERNS

- **NEVER** use HashMap for OpenAPI output (non-deterministic order)
- **NEVER** unwrap/panic in the collector or any macro path - return
  `MacroResult<T>` and surface the failure via `err_call_site` /
  `syn::Error::new_spanned`
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
