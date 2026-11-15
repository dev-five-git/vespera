//! `include/vespera.h` is hand-written (no `build.rs`, no cbindgen): this
//! test keeps it in lock-step with the Rust symbols.  The expected
//! declarations are not listed by hand: every `#[unsafe(no_mangle)]`
//! function in the crate's `src/` (the library's own exports plus the
//! `vespera_init` that `ffi_apps!` emits) and the `VesperaCallback` alias
//! are read from the Rust source and spelled as C, and that set must equal
//! the header's declarations — so a changed Rust signature, a new export
//! without a header prototype, or a stale prototype fails the test.  The
//! function-pointer coercions below additionally pin the source text this
//! test reads to the items the compiler builds.
#![cfg(feature = "ffi")]

use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};

use vespera_inprocess::ffi::{self, HEADER, VesperaCallback};

fn app() -> axum::Router {
    axum::Router::new()
}

// Emits `vespera_init` here so its signature is coerced below too.
vespera_inprocess::ffi_app!(app);

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
/// preprocessor line.  Only empty pieces and the closing `}` of the
/// `extern "C"` block are dropped, so any other declaration — whatever its
/// name — reaches the exact comparison below.
fn declarations(src: &str) -> Vec<String> {
    let code: String = strip_comments(src)
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    code.replace("extern \"C\" {", "")
        .split(';')
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty() && d != "}")
        .collect()
}

#[test]
fn a_declaration_without_the_vespera_prefix_is_not_skipped() {
    let src = "#ifdef __cplusplus
extern \"C\" {
#endif
               typedef struct Handle Handle;
int dispatch_raw(void);
               int vespera_init(void);
#ifdef __cplusplus
}
#endif
";
    assert_eq!(
        declarations(src),
        [
            "typedef struct Handle Handle",
            "int dispatch_raw(void)",
            "int vespera_init(void)"
        ]
    );
}

/// The header's declarations keyed by the name they declare (the
/// `(*name)` of a function-pointer typedef, else the identifier before the
/// first `(`).  Panics on a declaration that is neither, or on a name
/// declared twice.
fn header_decls(header: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for decl in declarations(header) {
        let name = match decl.split_once("(*") {
            Some((_, rest)) if decl.starts_with("typedef ") => rest.split(')').next().unwrap(),
            _ => decl[..decl
                .find('(')
                .unwrap_or_else(|| panic!("not a prototype: {decl}"))]
                .rsplit(' ')
                .next()
                .unwrap(),
        }
        .to_owned();
        assert!(
            out.insert(name.clone(), decl).is_none(),
            "{name} declared twice"
        );
    }
    out
}

#[test]
#[should_panic(expected = "not a prototype")]
fn a_declaration_that_is_not_a_prototype_is_not_skipped() {
    let _ = header_decls("typedef struct Handle Handle;");
}

/// A Rust FFI type as `vespera.h` spells it (`*` attached on the right,
/// `const` only on a pointee).  Panics on a type with no C spelling here,
/// so a new parameter type can never be compared loosely.
fn c_type(rust: &str) -> String {
    let rust = rust.trim();
    if let Some(inner) = rust.strip_prefix("*const ") {
        let inner = c_type(inner);
        assert!(!inner.ends_with('*'), "*const of a pointer: {rust}");
        return format!("const {inner} *");
    }
    if let Some(inner) = rust.strip_prefix("*mut ") {
        let inner = c_type(inner);
        return if inner.ends_with('*') {
            format!("{inner}*")
        } else {
            format!("{inner} *")
        };
    }
    match rust
        .trim_start_matches("::")
        .trim_start_matches("std::ffi::")
    {
        "u8" => "uint8_t",
        "u32" => "uint32_t",
        "u64" => "uint64_t",
        "usize" => "size_t",
        "c_int" => "int",
        "c_void" => "void",
        "Option<VesperaCallback>" => "vespera_callback",
        other => panic!("no C spelling for the Rust type {other:?}"),
    }
    .to_owned()
}

/// `(name: Type, ...) [-> Ret]` at the start of `sig` → (C parameter list,
/// C return type).
fn c_signature(sig: &str) -> (String, String) {
    let sig = sig.trim_start().strip_prefix('(').expect("missing '('");
    let close = sig.find(')').expect("missing ')'");
    let params: Vec<String> = sig[..close]
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, ty) = p.split_once(':').expect("parameter without a type");
            let ty = c_type(ty);
            let sep = if ty.ends_with('*') { "" } else { " " };
            format!("{ty}{sep}{}", name.trim())
        })
        .collect();
    let params = if params.is_empty() {
        "void".to_owned()
    } else {
        params.join(", ")
    };
    let ret = sig[close + 1..]
        .trim_start()
        .strip_prefix("->")
        .map_or_else(
            || "void".to_owned(),
            |r| c_type(&r[..r.find(['{', ';']).expect("unterminated signature")]),
        );
    (params, ret)
}

/// Every `#[unsafe(no_mangle)]` function in `src` spelled as its C
/// prototype, plus the `VesperaCallback` alias as the `vespera_callback`
/// typedef, keyed like [`header_decls`].  Panics on any other export form
/// (`#[no_mangle]`, `export_name`, an item that is not a `pub extern "C"
/// fn`) so the exported set is always complete.
fn rust_exports(src: &str) -> BTreeMap<String, String> {
    const ATTR: &str = "#[unsafe(no_mangle)]";
    assert_eq!(
        src.matches("no_mangle").count(),
        src.matches(ATTR).count(),
        "a no_mangle export in a form this test does not read"
    );
    assert!(!src.contains("export_name"), "an export_name export");
    let mut out = BTreeMap::new();
    for (at, _) in src.match_indices(ATTR) {
        let item = src[at + ATTR.len()..].trim_start();
        let item = item.strip_prefix("pub ").expect("non-pub export");
        let item = item.strip_prefix("unsafe ").unwrap_or(item);
        let item = item
            .strip_prefix("extern \"C\" fn ")
            .expect("export that is not an extern \"C\" fn");
        let name_end = item.find('(').expect("missing '('");
        let name = item[..name_end].trim();
        let (params, ret) = c_signature(&item[name_end..]);
        assert!(
            out.insert(name.to_owned(), format!("{ret} {name}({params})"))
                .is_none(),
            "{name} exported twice"
        );
    }
    let alias = src
        .split_once("pub type VesperaCallback =")
        .expect("VesperaCallback alias")
        .1;
    let alias = alias.trim_start().trim_start_matches("unsafe ");
    let alias = alias
        .strip_prefix("extern \"C\" fn")
        .expect("VesperaCallback is not an extern \"C\" fn");
    let (params, ret) = c_signature(alias);
    out.insert(
        "vespera_callback".to_owned(),
        format!("typedef {ret} (*vespera_callback)({params})"),
    );
    out
}

/// Text of every `.rs` file under the crate's `src/`, so an export added in
/// a new file is read too.
fn crate_sources() -> String {
    fn walk(dir: &std::path::Path, out: &mut String) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push_str(&std::fs::read_to_string(&path).unwrap());
                out.push('\n');
            }
        }
    }
    let mut out = String::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out
}

#[test]
fn header_declarations_equal_the_exported_rust_symbols() {
    let exports = rust_exports(&crate_sources());
    // The `ffi_apps!` expansion is read like the library's own exports.
    assert_eq!(exports["vespera_init"], "int vespera_init(void)");
    assert!(exports.contains_key("vespera_configure_default_json_content_type"));
    assert_eq!(header_decls(HEADER), exports);
}

#[test]
fn a_changed_rust_signature_no_longer_matches_the_header() {
    let src = crate_sources();
    let changed = src.replace(
        "vespera_free(ptr: *mut u8, len: usize)",
        "vespera_free(ptr: *mut u8, len: u32)",
    );
    assert_ne!(changed, src, "vespera_free signature not found");
    let exports = rust_exports(&changed);
    assert_eq!(
        exports["vespera_free"],
        "void vespera_free(uint8_t *ptr, uint32_t len)"
    );
    assert_ne!(header_decls(HEADER), exports);
}

#[test]
fn an_export_missing_from_the_header_no_longer_matches_it() {
    let src = format!(
        "{}\n#[unsafe(no_mangle)]\npub extern \"C\" fn vespera_configure_new_knob(value: usize) -> c_int {{}}\n",
        crate_sources()
    );
    let exports = rust_exports(&src);
    assert_eq!(
        exports["vespera_configure_new_knob"],
        "int vespera_configure_new_knob(size_t value)"
    );
    assert_ne!(header_decls(HEADER), exports);
}

#[rstest::rstest]
#[case::legacy_attribute("#[no_mangle]\npub extern \"C\" fn x() {}", "does not read")]
#[case::export_name(
    "#[unsafe(export_name = \"x\")]\npub extern \"C\" fn x() {}",
    "export_name export"
)]
#[case::unknown_type(
    "#[unsafe(no_mangle)]\npub extern \"C\" fn x(a: i16) {}",
    "no C spelling"
)]
#[case::not_a_fn("#[unsafe(no_mangle)]\npub static X: u8 = 0;", "not an extern")]
#[case::const_pointer_to_pointer(
    "#[unsafe(no_mangle)]\npub extern \"C\" fn x(a: *const *mut u8) {}",
    "*const of a pointer"
)]
fn an_unreadable_export_is_not_skipped(#[case] src: &str, #[case] expected: &str) {
    let src = format!("{src}\npub type VesperaCallback = unsafe extern \"C\" fn();");
    let panic = std::panic::catch_unwind(|| rust_exports(&src)).expect_err("accepted");
    let msg = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap();
    assert!(msg.contains(expected), "{msg}");
}

#[test]
fn header_symbols_coerce_to_their_c_prototypes() {
    // Compile-time: each symbol has exactly the shape the source text read
    // above describes (and the alias the `vespera_callback` typedef).
    let _: extern "C" fn() -> u32 = ffi::vespera_abi_version;
    let _: extern "C" fn() -> c_int = vespera_init;
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
    let _: extern "C" fn(c_int) -> c_int = ffi::vespera_configure_default_json_content_type;
    let callback: Option<VesperaCallback> = None;
    let _: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize)> = callback;
    // Every coerced symbol is one the source-derived comparison covers.
    assert_eq!(rust_exports(&crate_sources()).len(), 15);
}

/// A preprocessor line as `(directive, rest)`, whatever whitespace the
/// spelling puts after the `#` and the directive name (`#define X 1`,
/// `#<tab>define X 1`, `#  define<tab>X 1`); `None` for any other line.
fn directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix('#')?.trim_start();
    let (name, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((name, rest.trim()))
}

/// `#define VESPERA_* <int>` values.  Panics on any other `#define` — a
/// value that is not a (parenthesised) decimal integer, or a valueless one
/// besides the `VESPERA_H` include guard — and on any directive besides the
/// known guard / include / `__cplusplus` lines, so a new define can never be
/// skipped silently by the comparisons below (nor by [`declarations`],
/// which drops every `#` line).
fn defines(src: &str) -> BTreeMap<String, i64> {
    strip_comments(src)
        .lines()
        .filter_map(directive)
        .filter_map(|(kind, rest)| {
            if kind != "define" {
                assert!(
                    matches!(
                        (kind, rest),
                        ("ifndef", "VESPERA_H")
                            | ("include", "<stddef.h>" | "<stdint.h>")
                            | ("ifdef", "__cplusplus")
                            | ("endif", "")
                    ),
                    "unexpected directive #{kind} {rest}"
                );
                return None;
            }
            let Some((name, value)) = rest.split_once(char::is_whitespace) else {
                assert_eq!(rest, "VESPERA_H", "valueless #define");
                return None;
            };
            let value = value.trim().trim_start_matches('(').trim_end_matches(')');
            let parsed = value
                .parse()
                .unwrap_or_else(|_| panic!("#define {name} {value}: not a decimal integer"));
            Some((name.to_owned(), parsed))
        })
        .collect()
}

#[rstest::rstest]
#[case::plain("#define VESPERA_X 7")]
#[case::tab_after_define("#define\tVESPERA_X 7")]
#[case::space_after_hash("#  define VESPERA_X 7")]
#[case::tab_after_hash("#\tdefine VESPERA_X\t(7)")]
fn every_define_spelling_is_read(#[case] line: &str) {
    assert_eq!(defines(line), BTreeMap::from([("VESPERA_X".to_owned(), 7)]));
}

#[test]
#[should_panic(expected = "unexpected directive #undef VESPERA_OK")]
fn an_unknown_directive_is_not_skipped() {
    let _ = defines("#undef VESPERA_OK\n");
}

#[test]
#[should_panic(expected = "not a decimal integer")]
fn a_non_decimal_define_is_not_skipped() {
    let _ = defines("#define VESPERA_H\n#define VESPERA_FLAG 0x10\n");
}

#[test]
#[should_panic(expected = "valueless #define")]
fn a_valueless_define_other_than_the_guard_is_not_skipped() {
    let _ = defines("#define VESPERA_SOMETHING\n");
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
        (
            "VESPERA_MIN_OUT_BYTES",
            i64::try_from(vespera_inprocess::MIN_DIRECT_OUT_BYTES).unwrap(),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(defines(HEADER), expected);
}

/// The header comment's prose, `*` gutters removed and whitespace collapsed.
fn header_prose() -> String {
    HEADER
        .lines()
        .map(|l| l.trim().trim_start_matches("/*").trim_start_matches('*'))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The failure-list paragraph of the header documents the statuses the
/// engine synthesises, each with its cause, and the header-value paragraph
/// the Latin-1 mapping, so a C host can handle them without reading the Rust
/// sources.  The expected list is hand-maintained from the `error_wire` call
/// sites: a new synthesised status or cause must be added to both.  The
/// phrases are matched inside that paragraph only: the same numbers appear
/// elsewhere.
#[test]
fn header_documents_every_synthesised_status_and_the_latin1_mapping() {
    const START: &str = "Every failure that can be expressed as HTTP (";
    const END: &str = ") is returned as a normal wire response with VESPERA_OK.";
    let prose = header_prose();
    let list = &prose[prose.find(START).expect("failure list") + START.len()..];
    let list = &list[..list.find(END).expect("failure list end")];
    let entries: Vec<&str> = list.split(", ").collect();
    assert_eq!(
        entries,
        [
            "malformed wire / request 400",
            "unknown app 404",
            "invalid HTTP method 405",
            "body too large 413",
            "more than 1024 request headers 431",
            "handler panic 500 with metadata \"panicked\":\"true\"",
            "shared runtime could not be built 500",
            "response over the buffered response cap or not allocatable 500",
            "response body stream error 500",
            "response header too large 500",
            "runtime shut down 503",
            "deadline 504",
        ]
    );
    assert!(
        prose.contains(
            "HEADER VALUES use a Latin-1 mapping (one JSON char U+0000..U+00FF per byte)"
        )
    );
}

/// Sanity check of the shipped header's shape (include guard + C linkage
/// block); its contents are locked by the tests above.
#[test]
fn header_has_an_include_guard_and_c_linkage() {
    let header = HEADER.replace("\r\n", "\n");
    assert!(header.contains("#ifndef VESPERA_H\n#define VESPERA_H"));
    assert!(
        header.contains("extern \"C\" {") && header.trim_end().ends_with("#endif /* VESPERA_H */")
    );
}
