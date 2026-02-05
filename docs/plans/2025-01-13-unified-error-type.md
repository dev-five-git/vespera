# P0-1: Create Unified Error Type Module for vespera_macro

**Status:** Ready for Implementation  
**Priority:** P0 (Foundation)  
**Estimated Effort:** Medium  
**Date:** 2025-01-13

## Overview

Standardize all error handling in `vespera_macro` to use `syn::Error` exclusively. This enables proper span-based error reporting in proc-macros and removes the `anyhow` dependency.

## Problem Statement

Current error handling is inconsistent:
- `anyhow::Result` used in `collector.rs` and `file_utils.rs`
- `Result<_, String>` in `lib.rs:616`
- 32 production `.unwrap()` calls that can panic
- 1 `.expect()` call at `lib.rs:1065`
- Unprofessional error message: `"Failed to collect files from wtf: {}"` in `collector.rs:16`

## Solution

Create `src/error.rs` with:
1. `MacroResult<T>` type alias for `Result<T, syn::Error>`
2. Helper functions for creating errors with proper spans
3. `IntoSynError` trait for converting other error types

## Tasks

### Task 1: Create error.rs module

**File:** `crates/vespera_macro/src/error.rs`

```rust
//! Unified error handling for vespera_macro.
//!
//! All public APIs should return `MacroResult<T>` to ensure proper
//! span-based error reporting in proc-macros.

use proc_macro2::Span;
use syn::Error;

/// Result type for all macro operations.
pub type MacroResult<T> = Result<T, Error>;

/// Create an error at the given span.
#[inline]
pub fn err_spanned<T: quote::ToTokens, M: std::fmt::Display>(tokens: T, message: M) -> Error {
    Error::new_spanned(tokens, message)
}

/// Create an error at the call site.
#[inline]
pub fn err_call_site<M: std::fmt::Display>(message: M) -> Error {
    Error::new(Span::call_site(), message)
}

/// Trait for converting other error types to syn::Error.
pub trait IntoSynError {
    fn into_syn_error(self, span: Span) -> Error;
    fn into_syn_error_call_site(self) -> Error {
        self.into_syn_error(Span::call_site())
    }
}

impl IntoSynError for std::io::Error {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self.to_string())
    }
}

impl IntoSynError for String {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self)
    }
}

impl IntoSynError for &str {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self)
    }
}

impl IntoSynError for serde_json::Error {
    fn into_syn_error(self, span: Span) -> Error {
        Error::new(span, self.to_string())
    }
}

/// Extension trait for Result to convert errors with spans.
pub trait ResultExt<T, E> {
    fn map_syn_err(self, span: Span) -> MacroResult<T>;
    fn map_syn_err_call_site(self) -> MacroResult<T>;
}

impl<T, E: IntoSynError> ResultExt<T, E> for Result<T, E> {
    fn map_syn_err(self, span: Span) -> MacroResult<T> {
        self.map_err(|e| e.into_syn_error(span))
    }
    fn map_syn_err_call_site(self) -> MacroResult<T> {
        self.map_err(|e| e.into_syn_error_call_site())
    }
}

/// Extension trait for Option to convert to syn::Error.
pub trait OptionExt<T> {
    fn ok_or_syn_err<M: std::fmt::Display>(self, span: Span, message: M) -> MacroResult<T>;
    fn ok_or_syn_err_call_site<M: std::fmt::Display>(self, message: M) -> MacroResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_syn_err<M: std::fmt::Display>(self, span: Span, message: M) -> MacroResult<T> {
        self.ok_or_else(|| Error::new(span, message))
    }
    fn ok_or_syn_err_call_site<M: std::fmt::Display>(self, message: M) -> MacroResult<T> {
        self.ok_or_else(|| err_call_site(message))
    }
}
```

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 2: Register error module in lib.rs

**File:** `crates/vespera_macro/src/lib.rs`

Add near top with other module declarations:
```rust
mod error;
pub use error::{MacroResult, err_spanned, err_call_site, IntoSynError, ResultExt, OptionExt};
```

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 3: Update collector.rs - Replace anyhow with syn::Error

**File:** `crates/vespera_macro/src/collector.rs`

**Changes:**
1. Remove `use anyhow::Result;` (line 11)
2. Add `use crate::error::{MacroResult, ResultExt, err_call_site};`
3. Change function signatures from `Result<T>` to `MacroResult<T>`
4. Fix "wtf" message at line 16: `"Failed to collect files from wtf: {}"` → `"Failed to collect route files: {}"`
5. Convert `anyhow::anyhow!()` calls to `err_call_site()`
6. Use `.map_syn_err_call_site()` for IO errors

**Verification:**
- [ ] `cargo check -p vespera_macro`
- [ ] `cargo test -p vespera_macro`

---

### Task 4: Update file_utils.rs - Replace anyhow with syn::Error

**File:** `crates/vespera_macro/src/file_utils.rs`

**Changes:**
1. Remove `use anyhow::{anyhow, Result};` (line 4)
2. Add `use crate::error::{MacroResult, ResultExt, err_call_site};`
3. Change function signatures from `Result<T>` to `MacroResult<T>`
4. Convert `anyhow!()` calls to `err_call_site()`
5. Use `.map_syn_err_call_site()` for IO errors

**Verification:**
- [ ] `cargo check -p vespera_macro`
- [ ] `cargo test -p vespera_macro`

---

### Task 5: Fix lib.rs:616 Result<_, String>

**File:** `crates/vespera_macro/src/lib.rs`

**Location:** Line 616 (approximate)

**Change:** Replace `Result<_, String>` with `MacroResult<_>` and update error creation.

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 6: Fix .unwrap() calls in lib.rs (10 occurrences)

**File:** `crates/vespera_macro/src/lib.rs`

**Locations and fixes:**

| Line | Current | Fix |
|------|---------|-----|
| 121 | `SCHEMA_STORAGE.lock().unwrap()` | Keep - mutex poison is unrecoverable |
| 176 | `SCHEMA_STORAGE.lock().unwrap()` | Keep - mutex poison is unrecoverable |
| 244 | `SCHEMA_STORAGE.lock().unwrap()` | Keep - mutex poison is unrecoverable |
| 716 | `.unwrap()` | Analyze context, add `?` or `ok_or_syn_err()` |
| 725 | `.unwrap()` | Analyze context, add `?` or `ok_or_syn_err()` |
| 834 | `.unwrap()` | Analyze context, add `?` or `ok_or_syn_err()` |
| 836 | `.unwrap()` | Analyze context, add `?` or `ok_or_syn_err()` |
| 878 | `.unwrap()` | Analyze context, add `?` or `ok_or_syn_err()` |
| 880 | `.unwrap()` | Analyze context, add `?` or `ok_or_syn_err()` |
| 1064 | `SCHEMA_STORAGE.lock().unwrap()` | Keep - mutex poison is unrecoverable |

**Note:** Mutex `.lock().unwrap()` is acceptable - if a mutex is poisoned, the data is corrupted and panic is the correct behavior.

**Verification:**
- [ ] `cargo check -p vespera_macro`
- [ ] `cargo test -p vespera_macro`

---

### Task 7: Fix .expect() at lib.rs:1065

**File:** `crates/vespera_macro/src/lib.rs`

**Current:** `CARGO_MANIFEST_DIR.expect("CARGO_MANIFEST_DIR not set")`

**Fix:** This is actually acceptable - `CARGO_MANIFEST_DIR` is always set by Cargo during compilation. The expect message is clear. **Keep as-is.**

---

### Task 8: Fix .unwrap() calls in openapi_generator.rs (2 occurrences)

**File:** `crates/vespera_macro/src/openapi_generator.rs`

| Line | Current | Fix |
|------|---------|-----|
| 44 | `syn::parse_str().unwrap()` | Return error: `.map_syn_err_call_site()?` |
| 55 | `syn::parse_str().unwrap()` | Return error: `.map_syn_err_call_site()?` |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 9: Fix .unwrap() calls in parser/is_keyword_type.rs (1 occurrence)

**File:** `crates/vespera_macro/src/parser/is_keyword_type.rs`

| Line | Current | Fix |
|------|---------|-----|
| 39 | `segments.last().unwrap()` | Use `segments.last()?` if in Option context, or guard with `if let` |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 10: Fix .unwrap() calls in parser/operation.rs (4 occurrences)

**File:** `crates/vespera_macro/src/parser/operation.rs`

| Line | Current | Fix |
|------|---------|-----|
| 33 | `segments.last().unwrap()` | Add guard or use `ok_or_syn_err()` |
| 74 | `segments.last().unwrap()` | Add guard or use `ok_or_syn_err()` |
| 124 | `segments.last().unwrap()` | Add guard or use `ok_or_syn_err()` |
| 145 | `segments.last().unwrap()` | Add guard or use `ok_or_syn_err()` |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 11: Fix .unwrap() calls in parser/parameters.rs (6 occurrences)

**File:** `crates/vespera_macro/src/parser/parameters.rs`

| Line | Current | Fix |
|------|---------|-----|
| 67 | `.unwrap()` | Use `ok_or_syn_err()` or pattern match |
| 77 | `.unwrap()` | Use `ok_or_syn_err()` or pattern match |
| 101 | `.unwrap()` | Use `ok_or_syn_err()` or pattern match |
| 279 | `.unwrap()` | Use `ok_or_syn_err()` or pattern match |
| 323 | `.unwrap()` | Use `ok_or_syn_err()` or pattern match |
| 363 | `.unwrap()` | Use `ok_or_syn_err()` or pattern match |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 12: Fix .unwrap() calls in parser/request_body.rs (1 occurrence)

**File:** `crates/vespera_macro/src/parser/request_body.rs`

| Line | Current | Fix |
|------|---------|-----|
| 34 | `segments.last().unwrap()` | Use `ok_or_syn_err()` or guard |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 13: Fix .unwrap() calls in parser/response.rs (2 occurrences)

**File:** `crates/vespera_macro/src/parser/response.rs`

| Line | Current | Fix |
|------|---------|-----|
| 17 | `segments.last().unwrap()` | Use `ok_or_syn_err()` or guard |
| 75 | `segments.last().unwrap()` | Use `ok_or_syn_err()` or guard |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 14: Fix .unwrap() calls in schema_macro/mod.rs (4 occurrences)

**File:** `crates/vespera_macro/src/schema_macro/mod.rs`

| Line | Current | Fix |
|------|---------|-----|
| 348 | field ident `.unwrap()` | Use `ok_or_syn_err()` with field span |
| 440 | field ident `.unwrap()` | Use `ok_or_syn_err()` with field span |
| 462 | field ident `.unwrap()` | Use `ok_or_syn_err()` with field span |
| 500 | field ident `.unwrap()` | Use `ok_or_syn_err()` with field span |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 15: Fix .unwrap() calls in schema_macro/file_lookup.rs (1 occurrence)

**File:** `crates/vespera_macro/src/schema_macro/file_lookup.rs`

| Line | Current | Fix |
|------|---------|-----|
| 227 | iterator `.next().unwrap()` | Use `ok_or_syn_err_call_site()` or guard |

**Verification:**
- [ ] `cargo check -p vespera_macro`

---

### Task 16: Remove anyhow dependency

**File:** `crates/vespera_macro/Cargo.toml`

**Change:** Remove `anyhow` from `[dependencies]`

**Verification:**
- [ ] `cargo check -p vespera_macro`
- [ ] `cargo test -p vespera_macro`
- [ ] `cargo clippy -p vespera_macro -- -D warnings`

---

### Task 17: Final verification

**Commands:**
```bash
cd crates/vespera_macro
cargo check
cargo test
cargo clippy -- -D warnings

# Full workspace verification
cd ../..
cargo check --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

**Verification:**
- [ ] All checks pass
- [ ] All tests pass
- [ ] No clippy warnings
- [ ] No `anyhow` in dependency tree for vespera_macro

---

## Implementation Order

1. Task 1-2: Create error module and register it
2. Task 3-5: Update collector.rs, file_utils.rs, and lib.rs string error
3. Task 6-15: Fix all .unwrap() calls (can be parallelized by file)
4. Task 16: Remove anyhow
5. Task 17: Final verification

## Notes

- **Mutex unwrap()**: Keeping `SCHEMA_STORAGE.lock().unwrap()` is intentional - mutex poisoning indicates corrupted state and panic is correct.
- **CARGO_MANIFEST_DIR expect()**: Keeping as-is - this env var is always set by Cargo.
- **Span preservation**: When converting errors, preserve the original span when possible for better error messages.

## Success Criteria

- [ ] No `anyhow` dependency in vespera_macro
- [ ] All errors use `syn::Error` with proper spans
- [ ] No production `.unwrap()` except mutex locks
- [ ] Professional error messages only
- [ ] All tests pass
- [ ] Clippy clean
