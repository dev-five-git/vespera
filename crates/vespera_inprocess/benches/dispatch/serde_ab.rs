//! `bench-support`-gated within-run A/B benchmark groups.
//!
//! Each group compares a production hand-rolled path against its retained
//! `serde_json` / `http::request::Builder` / `serde_json::Value` "before" twin
//! in the SAME criterion run (noise-robust). Split out of `dispatch.rs` to keep
//! that file under the 1000-line cap; the whole module is compiled only under
//! `--features bench-support` (the `mod` declaration in `dispatch.rs` is
//! `#[cfg(feature = "bench-support")]`). Wired into the parent `ab_benches`
//! criterion group.

// This is a `#[path]` bench submodule of `dispatch.rs`; it intentionally
// re-uses the parent bench file's imports (criterion types, header types,
// wire-assembly helpers) rather than re-listing them.  The glob is the
// idiomatic shape for a bench helper split out only to honour the file-size cap.
#[allow(clippy::wildcard_imports)]
use super::*;

/// `request_parse_*` / `response_serialize_*` within-run A/B: the hand-rolled
/// wire-header parse / slice-serialize vs the retained `serde_json` twins, in
/// the SAME criterion run so the delta is read without cross-run drift.
///
/// - `request_parse_*`: full header parse of a realistic small
///   `GET /health`-shaped header (the SmartDispatch DIRECT sweet spot) —
///   `parse_wire_header` (hand) vs `parse_wire_header_serde`.
/// - `response_serialize_*`: slice-serialize of a many-header response
///   (10 single-value + 3-value `set-cookie` + content-type/length) —
///   `write_wire_header_into_slice` (hand) vs the `serde_json` twin.
pub fn bench_wire_header_serde(c: &mut Criterion) {
    use vespera_inprocess::ResponseMetadata;
    use vespera_inprocess::bench_support::{
        bench_parse_hand, bench_parse_serde, bench_write_hand, bench_write_serde,
    };

    // Request-parse fixture: exactly the JSON object `parse_wire_header`
    // receives (no length prefix) for a small idempotent GET.
    let request_header: &[u8] = br#"{"v":1,"method":"GET","path":"/health","headers":{"accept":"*/*","user-agent":"bench/1.0","host":"localhost:3000"}}"#;

    // Forward-compat fixture: the same small GET plus UNKNOWN header fields
    // (an object with escaped-string values + nesting, and an array). These
    // are ignored by both parsers via the value-skip path — the input shape
    // a newer client / custom FFI caller can legitimately send. Isolates the
    // unknown-value skip cost (escaped-string skip allocation + the recursion
    // depth guard) that the standard `request_header` fixture never exercises.
    let request_header_unknown: &[u8] = br#"{"v":1,"method":"GET","path":"/health","headers":{"accept":"*/*"},"x-meta":{"trace":"a\"b\nc\td","span":"00f0\u00e9","nested":{"k":[1,2,"v\u00e9"]}},"flags":[true,null,42,-3.14e2]}"#;

    // Response-serialize fixture: the realistic many-header response shape
    // (mirrors `handler_many_headers`) plus content-type / content-length.
    let mut resp_headers = HeaderMap::new();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("etag", "\"abc123def456\""),
        ("vary", "accept-encoding"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("x-request-id", "01HV2N3M4P5Q6R7S8T9V0W1X2Y"),
        ("x-trace-id", "4bf92f3577b34da6a3ce929d0e0e4736"),
        ("access-control-allow-origin", "*"),
        ("strict-transport-security", "max-age=63072000"),
        ("content-language", "en"),
        ("content-type", "application/json"),
        ("content-length", "1024"),
    ] {
        resp_headers.insert(
            HeaderName::from_static(name),
            value.parse().expect("static header value"),
        );
    }
    let cookie = HeaderName::from_static("set-cookie");
    resp_headers.append(cookie.clone(), "session=s1; HttpOnly".parse().unwrap());
    resp_headers.append(cookie.clone(), "theme=dark; Path=/".parse().unwrap());
    resp_headers.append(cookie, "lang=en; Path=/".parse().unwrap());
    let metadata = ResponseMetadata::current();

    let mut group = c.benchmark_group("wire_header_serde");

    group.bench_function("request_parse_hand", |b| {
        b.iter(|| bench_parse_hand(std::hint::black_box(request_header)));
    });
    group.bench_function("request_parse_serde", |b| {
        b.iter(|| bench_parse_serde(std::hint::black_box(request_header)));
    });

    // Forward-compat unknown-field skip path (escaped-string skip + depth
    // guard). Standard `request_parse_hand` never enters `skip_value`, so this
    // is where the non-allocating escaped-string skip shows up.
    group.bench_function("request_parse_unknown_hand", |b| {
        b.iter(|| bench_parse_hand(std::hint::black_box(request_header_unknown)));
    });
    group.bench_function("request_parse_unknown_serde", |b| {
        b.iter(|| bench_parse_serde(std::hint::black_box(request_header_unknown)));
    });

    // Size the out buffer once (outside the timed loop) and reuse it,
    // mirroring the pooled direct buffer the JNI bridge hands in.
    let required = bench_write_hand(&mut [0u8; 1024], 200, &resp_headers, &metadata);
    group.bench_function("response_serialize_hand", |b| {
        let mut out = vec![0u8; required];
        b.iter(|| bench_write_hand(&mut out, 200, &resp_headers, &metadata));
    });
    group.bench_function("response_serialize_serde", |b| {
        let mut out = vec![0u8; required];
        b.iter(|| bench_write_serde(&mut out, 200, &resp_headers, &metadata));
    });

    group.finish();
}

/// Direct `Request<Body>` construction vs the `http::request::Builder` state
/// machine (within-run A/B).  Both arms build a full request from the same
/// method / path / query / headers / body in the SAME criterion run
/// (noise-robust, like `wire_header_serde`), so the builder-vs-direct delta is
/// read without cross-run drift.  Each arm sums the built request's field byte
/// lengths so neither can be optimised down to a partial build.
///
/// Fixtures span the dispatch hot path's real request shapes: a bodyless `GET`
/// (the DIRECT sweet spot), a `GET` with 3 headers, a small `POST` with
/// `content-type`, and a `POST` with 8 realistic headers.
pub fn bench_request_build_path(c: &mut Criterion) {
    use vespera_inprocess::bench_support::{bench_build_request_new, bench_build_request_old};

    type Fixture = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static str,
    );
    let fixtures: &[Fixture] = &[
        ("bodyless_get", "GET", "/r0", "", &[], ""),
        (
            "get_3_headers",
            "GET",
            "/r0",
            "",
            &[
                ("accept", "*/*"),
                ("user-agent", "bench/1.0"),
                ("host", "localhost:3000"),
            ],
            "",
        ),
        (
            "post_content_type",
            "POST",
            "/echo",
            "",
            &[("content-type", "application/json")],
            r#"{"body":"x"}"#,
        ),
        (
            "post_8_headers",
            "POST",
            "/echo",
            "",
            &[
                ("content-type", "application/json"),
                ("accept", "*/*"),
                ("user-agent", "bench/1.0"),
                ("host", "localhost:3000"),
                ("authorization", "Bearer abcdef0123456789"),
                ("accept-encoding", "gzip, deflate, br"),
                ("accept-language", "en-US,en;q=0.9"),
                ("x-request-id", "01HV2N3M4P5Q6R7S8T9V0W1X2Y"),
            ],
            r#"{"body":"x"}"#,
        ),
    ];

    let mut group = c.benchmark_group("request_build_ab");
    for &(label, method, path, query, headers, body) in fixtures {
        let body = bytes::Bytes::copy_from_slice(body.as_bytes());
        group.bench_function(BenchmarkId::new("direct_new", label), |b| {
            b.iter(|| bench_build_request_new(method, path, query, headers, body.clone()));
        });
        group.bench_function(BenchmarkId::new("builder_old", label), |b| {
            b.iter(|| bench_build_request_old(method, path, query, headers, body.clone()));
        });
    }
    group.finish();
}

/// `metadata_segment_*` within-run A/B: the pre-baked metadata-segment fast
/// path vs the general `write_json_string` path, both invoked on the SAME
/// small `HeaderMap` + status fixture in the SAME criterion run so the
/// per-response metadata-block delta is read without cross-run drift.
///
/// - `metadata_static_fast`: `ResponseMetadata::current()` — the
///   `Cow::Borrowed(env!("CARGO_PKG_VERSION"))` pointer that the production
///   fast path detects (one bulk `sink.put` of the pre-baked const segment).
/// - `metadata_owned_general`: `ResponseMetadata { version: Cow::Owned(...) }`
///   over the SAME version bytes — forces the general path (three `sink.put`
///   calls + per-byte ESCAPE scan).
///
/// Fixture: a small `HeaderMap` with one `content-type` entry + `status:200`,
/// matching the single-content-type response shape that dominates the DIRECT
/// dispatch hot path.  Output is byte-identical between the two arms (SemVer
/// never trips the ESCAPE table), so the delta is the metadata block's
/// scan + extra `sink.put`s — exactly what the fast path skips.  The decision
/// guard: if `metadata_static_fast` does NOT beat `metadata_owned_general` on
/// the median of 5 runs the production fast-path-detect is providing no
/// measurable benefit and should be reverted.
pub fn bench_metadata_segment(c: &mut Criterion) {
    use std::borrow::Cow;
    use vespera_inprocess::ResponseMetadata;
    use vespera_inprocess::bench_support::bench_write_hand;

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("content-type"),
        "application/json".parse().expect("static header value"),
    );

    // Production fast path: borrowed `'static` SemVer (pointer-eq vs
    // `VESPERA_VERSION` in `wire/header_write.rs`).
    let meta_current = ResponseMetadata::current();
    // General path: SAME bytes but `Cow::Owned`, so the production fast-path
    // detect fails and the existing `write_json_string` block runs.
    let meta_owned = ResponseMetadata {
        version: Cow::Owned(env!("CARGO_PKG_VERSION").to_owned()),
    };

    // Size the out buffer once outside the timed loop (mirrors the pooled
    // direct buffer the JNI bridge hands in).
    let required = bench_write_hand(&mut [0u8; 256], 200, &headers, &meta_current);

    let mut group = c.benchmark_group("metadata_segment_ab");
    group.bench_function("metadata_static_fast", |b| {
        let mut out = vec![0u8; required];
        b.iter(|| bench_write_hand(&mut out, 200, &headers, &meta_current));
    });
    group.bench_function("metadata_owned_general", |b| {
        let mut out = vec![0u8; required];
        b.iter(|| bench_write_hand(&mut out, 200, &headers, &meta_owned));
    });
    group.finish();
}

/// Typed-deserialize vs `serde_json::Value` DOM for the 422 validation-error
/// hoist (within-run A/B).  Both arms parse the same framework-generated
/// `{"errors":[{"path","message"}]}` envelope in the SAME criterion run, so
/// the DOM-removal delta is read without cross-run drift.  Each arm sums the
/// hoisted field byte lengths so neither can be optimised to a partial parse.
///
/// Fixtures: a 1-error envelope (typical single-field failure) and a 5-error
/// envelope (form-heavy request) — where the eliminated `Value` map/array/key
/// allocations scale with error count.
pub fn bench_hoist_422_path(c: &mut Criterion) {
    use vespera_inprocess::bench_support::{bench_hoist_new, bench_hoist_old};

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("content-type"),
        "application/json".parse().expect("static header value"),
    );

    let body_1: &str = r#"{"errors":[{"path":"email","message":"not a valid email"}]}"#;
    let body_5: &str = r#"{"errors":[{"path":"username","message":"length is lower than 3"},{"path":"email","message":"not a valid email"},{"path":"age","message":"greater than 120"},{"path":"bio","message":"length is greater than 256"},{"path":"phone","message":"not a valid phone number"}]}"#;

    let mut group = c.benchmark_group("hoist_422_ab");
    for (label, body) in [("errors_1", body_1), ("errors_5", body_5)] {
        let body = bytes::Bytes::copy_from_slice(body.as_bytes());
        group.bench_function(BenchmarkId::new("typed_new", label), |b| {
            b.iter(|| bench_hoist_new(&headers, &body));
        });
        group.bench_function(BenchmarkId::new("value_old", label), |b| {
            b.iter(|| bench_hoist_old(&headers, &body));
        });
    }
    group.finish();
}
