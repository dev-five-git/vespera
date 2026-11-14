//! `include/vespera.h` is hand-written (no `build.rs`, no cbindgen): this
//! test keeps it in lock-step with the Rust symbols — every prototype and
//! constant in the header is listed here, and each Rust symbol is coerced
//! to the function-pointer type the prototype describes, so a signature
//! change on either side fails the build or the test.
#![cfg(feature = "ffi")]

use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};

use vespera_inprocess::ffi::{self, HEADER, VesperaCallback};

/// Header text with comments removed.
fn strip_comments(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find("*/").expect("unterminated comment");
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}

/// Every `;`-terminated declaration (whitespace-normalised) that is not a
/// preprocessor line.
fn declarations(src: &str) -> Vec<String> {
    let code: String = strip_comments(src)
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    code.replace("extern \"C\" {", "")
        .split(';')
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| d.contains("vespera"))
        .collect()
}

/// `#define VESPERA_* <int>` values.
fn defines(src: &str) -> BTreeMap<String, i64> {
    strip_comments(src)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("#define "))
        .filter_map(|l| {
            let (name, value) = l.split_once(' ')?;
            let value = value.trim().trim_start_matches('(').trim_end_matches(')');
            Some((name.to_owned(), value.parse().ok()?))
        })
        .collect()
}

#[test]
fn header_prototypes_match_the_exported_symbols() {
    // Compile-time: each symbol has exactly the C prototype's shape.
    let _: extern "C" fn() -> u32 = ffi::vespera_abi_version;
    let _: unsafe extern "C" fn(*const u8, usize, *mut *mut u8, *mut usize) -> c_int =
        ffi::vespera_dispatch;
    let _: unsafe extern "C" fn(*const u8, usize, *mut u8, usize, *mut usize) -> c_int =
        ffi::vespera_dispatch_into;
    let _: unsafe extern "C" fn(*const u8, usize, Option<VesperaCallback>, *mut c_void) -> c_int =
        ffi::vespera_dispatch_async;
    let _: unsafe extern "C" fn(*mut u8, usize) = ffi::vespera_free;
    let _: extern "C" fn(u64) -> c_int = ffi::vespera_shutdown;
    let _: extern "C" fn(usize) -> c_int = ffi::vespera_configure_worker_threads;
    let _: extern "C" fn(usize) -> c_int = ffi::vespera_configure_max_blocking_threads;
    let _: extern "C" fn(usize) -> c_int = ffi::vespera_configure_max_request_bytes;
    let _: extern "C" fn(usize) -> c_int = ffi::vespera_configure_max_buffered_response_bytes;
    let _: extern "C" fn(u64) -> c_int = ffi::vespera_configure_dispatch_timeout_ms;
    let _: extern "C" fn(usize) -> c_int = ffi::vespera_configure_async_max_in_flight;
    let callback: Option<VesperaCallback> = None;
    let _: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize)> = callback;

    assert_eq!(
        declarations(HEADER),
        [
            "typedef void (*vespera_callback)(void *user_data, uint8_t *response, size_t response_len)",
            "uint32_t vespera_abi_version(void)",
            "int vespera_init(void)",
            "int vespera_dispatch(const uint8_t *request, size_t request_len, uint8_t **response, size_t *response_len)",
            "int vespera_dispatch_into(const uint8_t *request, size_t request_len, uint8_t *out, size_t out_capacity, size_t *written)",
            "int vespera_dispatch_async(const uint8_t *request, size_t request_len, vespera_callback callback, void *user_data)",
            "void vespera_free(uint8_t *ptr, size_t len)",
            "int vespera_shutdown(uint64_t timeout_ms)",
            "int vespera_configure_worker_threads(size_t worker_threads)",
            "int vespera_configure_max_blocking_threads(size_t max_blocking_threads)",
            "int vespera_configure_max_request_bytes(size_t max_bytes)",
            "int vespera_configure_max_buffered_response_bytes(size_t max_bytes)",
            "int vespera_configure_dispatch_timeout_ms(uint64_t timeout_ms)",
            "int vespera_configure_async_max_in_flight(size_t max_in_flight)",
        ]
    );
}

#[test]
fn header_constants_match_the_rust_constants() {
    let expected: BTreeMap<String, i64> = [
        ("VESPERA_ABI_VERSION", i64::from(ffi::VESPERA_ABI_VERSION)),
        ("VESPERA_OK", i64::from(ffi::VESPERA_OK)),
        ("VESPERA_OVERFLOW", i64::from(ffi::VESPERA_OVERFLOW)),
        (
            "VESPERA_ERR_INVALID_ARGUMENT",
            i64::from(ffi::VESPERA_ERR_INVALID_ARGUMENT),
        ),
        ("VESPERA_ERR_PANIC", i64::from(ffi::VESPERA_ERR_PANIC)),
        (
            "VESPERA_ERR_REGISTRATION",
            i64::from(ffi::VESPERA_ERR_REGISTRATION),
        ),
        ("VESPERA_ERR_RUNTIME", i64::from(ffi::VESPERA_ERR_RUNTIME)),
        ("VESPERA_ERR_TOO_LATE", i64::from(ffi::VESPERA_ERR_TOO_LATE)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(defines(HEADER), expected);
}

/// The header's prose documents every status the engine synthesises (the
/// `error_wire` call sites) and the header-value Latin-1 mapping, so a C
/// host can handle them without reading the Rust sources.
#[test]
fn header_documents_every_synthesised_status_and_the_latin1_mapping() {
    for documented in [
        "400",
        "404",
        "405",
        "413",
        "431",
        "\"panicked\":\"true\"",
        "could not be built 500",
        "503",
        "504",
        "Latin-1",
    ] {
        assert!(
            HEADER.replace("\r\n", "\n").contains(documented),
            "vespera.h does not document {documented:?}"
        );
    }
}

#[test]
fn header_matches_the_shipped_file() {
    let on_disk =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/include/vespera.h")).unwrap();
    assert_eq!(on_disk.replace("\r\n", "\n"), HEADER.replace("\r\n", "\n"));
    assert!(HEADER.contains("#ifndef VESPERA_H") && HEADER.contains("extern \"C\""));
}
