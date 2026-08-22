//! Integration test for [`vespera::multipart::TypedMultipart`] over the
//! binary wire ([`vespera_inprocess::dispatch_from_bytes`]).
//!
//! Pins the contract that real-world `multipart/form-data` file uploads
//! pass through the binary wire envelope **byte-for-byte intact** —
//! `boundary`-delimited parts arrive at the `TypedMultipart<T>`
//! extractor exactly as constructed.
//!
//! Includes a 256 KiB payload to exercise the
//! `axum::extract::DefaultBodyLimit::disable()` layer; without it
//! axum's default 2 MiB cap would silently truncate larger uploads.

use ::axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, FromRequest},
    http::{HeaderMap, HeaderValue, Request},
    routing::post,
};
use ::serde::Serialize;
use ::serde_json::Value;
use ::std::collections::HashMap;
use ::std::io::{Read, Seek, SeekFrom};
use ::std::sync::Once;
use ::tokio::runtime::Builder;
use ::vespera::axum::Json;
use ::vespera::multipart::{
    DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES, FieldData, FieldMetadata, MeteredField,
    TryFromFieldWithState, TypedMultipart, TypedMultipartError, TypedMultipartWithLimits,
};
use ::vespera::tempfile::NamedTempFile;
use ::vespera::{Multipart, Schema, Validated};
use ::vespera_inprocess::{dispatch_from_bytes, register_app};

#[derive(Multipart, Schema)]
#[allow(dead_code)]
struct UploadReq {
    name: String,
    // This round-trip test intentionally accepts any size (it exercises the
    // 256 KiB tempfile path with the body limit disabled), so it opts out of
    // the now-mandatory file-field cap explicitly rather than inheriting one.
    #[form_data(limit = "unlimited")]
    file: FieldData<NamedTempFile>,
}

#[derive(Multipart, Schema)]
#[allow(dead_code)]
struct CappedUploadReq {
    name: String,
    file: FieldData<NamedTempFile>,
}

#[derive(Multipart, Schema, garde::Validate)]
#[allow(dead_code)]
struct ValidatedMultipartReq {
    #[garde(length(min = 3))]
    name: String,
}

#[derive(Multipart)]
struct BooleanReq {
    enabled: bool,
}

struct ObservedField {
    file_name: Option<String>,
    content_type: Option<String>,
    bytes: Vec<u8>,
    captured_header: Option<String>,
}

impl<S: Send + Sync> TryFromFieldWithState<S> for ObservedField {
    async fn try_from_field_with_state(
        field: MeteredField<'_>,
        _limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let file_name = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(str::to_owned);
        let mut headers = HeaderMap::new();
        headers.insert("x-observed", HeaderValue::from_static("yes"));
        let metadata = FieldMetadata::from(&field).with_headers(headers);
        let captured_header = metadata
            .headers()
            .and_then(|headers| headers.get("x-observed"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = field.bytes().await?.to_vec();
        Ok(Self {
            file_name,
            content_type,
            bytes,
            captured_header,
        })
    }
}

#[derive(Multipart)]
struct ObservedReq {
    file: ObservedField,
}

struct FourByteField(Vec<u8>);

impl<S: Send + Sync> TryFromFieldWithState<S> for FourByteField {
    async fn try_from_field_with_state(
        field: MeteredField<'_>,
        _limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        Ok(Self(field.bytes_with_limit(4, 64).await?.to_vec()))
    }
}

#[derive(Multipart)]
struct LimitedReq {
    value: FourByteField,
}

#[derive(Serialize, Schema)]
struct UploadResult {
    name: String,
    file_size: u64,
    file_first_byte: u8,
    file_last_byte: u8,
}

async fn upload_handler(TypedMultipart(mut req): TypedMultipart<UploadReq>) -> Json<UploadResult> {
    let mut buf = Vec::new();
    let f = req.file.contents.as_file_mut();
    // multipart parser leaves the file cursor at EOF after writing
    f.seek(SeekFrom::Start(0)).expect("rewind temp file");
    f.read_to_end(&mut buf).expect("read temp file");
    let len = u64::try_from(buf.len()).expect("file size fits in u64");
    let first = *buf.first().unwrap_or(&0);
    let last = *buf.last().unwrap_or(&0);
    Json(UploadResult {
        name: req.name,
        file_size: len,
        file_first_byte: first,
        file_last_byte: last,
    })
}

async fn capped_upload_handler(
    TypedMultipart(mut req): TypedMultipart<CappedUploadReq>,
) -> Json<UploadResult> {
    let mut buf = Vec::new();
    let f = req.file.contents.as_file_mut();
    f.seek(SeekFrom::Start(0)).expect("rewind temp file");
    f.read_to_end(&mut buf).expect("read temp file");
    Json(UploadResult {
        name: req.name,
        file_size: u64::try_from(buf.len()).expect("file size fits in u64"),
        file_first_byte: *buf.first().unwrap_or(&0),
        file_last_byte: *buf.last().unwrap_or(&0),
    })
}

async fn validated_multipart_handler(
    Validated(TypedMultipart(req)): Validated<TypedMultipart<ValidatedMultipartReq>>,
) -> Json<TextResult> {
    Json(TextResult {
        text_len: u64::try_from(req.name.len()).unwrap_or(u64::MAX),
    })
}

/// Unannotated `String` field — inherits the default 1 MiB cap.
#[derive(Multipart, Schema)]
#[allow(dead_code)]
struct TextReq {
    text: String,
}

/// Explicit `unlimited` opt-out — the field stays genuinely unbounded.
#[derive(Multipart, Schema)]
#[allow(dead_code)]
struct UnlimitedTextReq {
    #[form_data(limit = "unlimited")]
    text: String,
}

#[derive(Serialize, Schema)]
struct TextResult {
    text_len: u64,
}

#[derive(Serialize)]
struct BooleanResult {
    enabled: bool,
}

#[derive(Serialize)]
struct ObservedResult {
    file_name: Option<String>,
    content_type: Option<String>,
    byte_count: usize,
    captured_header: Option<String>,
}

async fn text_handler(TypedMultipart(req): TypedMultipart<TextReq>) -> Json<TextResult> {
    Json(TextResult {
        text_len: u64::try_from(req.text.len()).unwrap_or(u64::MAX),
    })
}

async fn text_aggregate_total_handler(
    TypedMultipartWithLimits(req): TypedMultipartWithLimits<TextReq, 8, 8>,
) -> Json<TextResult> {
    Json(TextResult {
        text_len: u64::try_from(req.text.len()).unwrap_or(u64::MAX),
    })
}

async fn text_aggregate_field_count_handler(
    TypedMultipartWithLimits(req): TypedMultipartWithLimits<TextReq, 1024, 0>,
) -> Json<TextResult> {
    Json(TextResult {
        text_len: u64::try_from(req.text.len()).unwrap_or(u64::MAX),
    })
}

async fn text_unlimited_handler(
    TypedMultipart(req): TypedMultipart<UnlimitedTextReq>,
) -> Json<TextResult> {
    Json(TextResult {
        text_len: u64::try_from(req.text.len()).unwrap_or(u64::MAX),
    })
}

async fn boolean_handler(TypedMultipart(req): TypedMultipart<BooleanReq>) -> Json<BooleanResult> {
    Json(BooleanResult {
        enabled: req.enabled,
    })
}

async fn observed_handler(
    TypedMultipart(req): TypedMultipart<ObservedReq>,
) -> Json<ObservedResult> {
    Json(ObservedResult {
        file_name: req.file.file_name,
        content_type: req.file.content_type,
        byte_count: req.file.bytes.len(),
        captured_header: req.file.captured_header,
    })
}

async fn limited_handler(TypedMultipart(req): TypedMultipart<LimitedReq>) -> Json<TextResult> {
    Json(TextResult {
        text_len: u64::try_from(req.value.0.len()).unwrap_or(u64::MAX),
    })
}

fn multipart_router() -> Router {
    Router::new()
        .route("/upload", post(upload_handler))
        .route("/capped-upload", post(capped_upload_handler))
        .route("/validated-multipart", post(validated_multipart_handler))
        .route("/text", post(text_handler))
        .route("/text-aggregate-total", post(text_aggregate_total_handler))
        .route(
            "/text-aggregate-field-count",
            post(text_aggregate_field_count_handler),
        )
        .route("/text-unlimited", post(text_unlimited_handler))
        .route("/boolean", post(boolean_handler))
        .route("/observed", post(observed_handler))
        .route("/limited", post(limited_handler))
        // Disable the 2 MiB default so the 256 KiB test below isn't
        // truncated — and so end-users can document a sensible policy
        // explicitly rather than inheriting an axum default that's
        // surprising in an in-process / JNI context.  The default String
        // field cap is enforced by vespera itself, independent of this.
        .layer(DefaultBodyLimit::disable())
}

fn install_router_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| register_app(multipart_router));
}

fn encode_multipart_wire(
    boundary: &str,
    name: &str,
    file_name: &str,
    file_bytes: &[u8],
) -> Vec<u8> {
    encode_multipart_upload_wire("/upload", boundary, name, file_name, file_bytes)
}

fn encode_multipart_upload_wire(
    path: &str,
    boundary: &str,
    name: &str,
    file_name: &str,
    file_bytes: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"name\"\r\n\r\n");
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_owned(),
        format!("multipart/form-data; boundary={boundary}"),
    );
    let header_json = ::serde_json::json!({
        "v": 1,
        "method": "POST",
        "path": path,
        "headers": headers,
    });
    let header_bytes = ::serde_json::to_vec(&header_json).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(&body);
    wire
}

fn decode_wire(resp: &[u8]) -> (Value, Vec<u8>) {
    assert!(resp.len() >= 4, "wire response too short");
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(4 + header_len <= resp.len(), "header_len overflow");
    let header: Value =
        ::serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

#[test]
fn typed_multipart_small_binary_roundtrip() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let payload: Vec<u8> = (0u32..1024)
        .map(|i| u8::try_from(i % 256).expect("mod 256"))
        .collect();
    let wire = encode_multipart_wire("----TestBoundary12345", "alice", "data.bin", &payload);
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);

    assert_eq!(
        header["status"].as_u64(),
        Some(200),
        "expected 200, got header={header:#}"
    );
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["name"], "alice");
    assert_eq!(json["file_size"], 1024);
    assert_eq!(json["file_first_byte"], 0);
    assert_eq!(json["file_last_byte"], 255);
}

#[test]
fn typed_multipart_large_binary_roundtrip() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // 256 KiB - exercises FieldData<NamedTempFile> tempfile path AND
    // proves DefaultBodyLimit::disable() works; without it the default
    // 2 MiB cap would only catch this above 2 MiB, but the existence
    // of this layer is the contract under test.
    let size: u32 = 256 * 1024;
    let payload: Vec<u8> = (0..size)
        .map(|i| u8::try_from(i % 256).expect("mod 256"))
        .collect();
    let wire = encode_multipart_wire("----LargeBoundary", "report", "big.bin", &payload);
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);

    assert_eq!(
        header["status"].as_u64(),
        Some(200),
        "expected 200, got header={header:#}"
    );
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["file_size"].as_u64(), Some(u64::from(size)));
}

#[test]
fn typed_multipart_non_utf8_bytes_preserved() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Mixed invalid UTF-8 + binary signatures — proves the wire passes
    // bytes verbatim with NO UTF-8 coercion anywhere in the pipeline.
    let payload: Vec<u8> = vec![0x00, 0xFF, 0xC0, 0xC0, 0xDE, 0xAD, 0xBE, 0xEF];
    let wire = encode_multipart_wire("----NonUtf8Boundary", "bin", "raw.bin", &payload);
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);

    assert_eq!(
        header["status"].as_u64(),
        Some(200),
        "expected 200, got header={header:#}"
    );
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["file_size"], 8);
    assert_eq!(json["file_first_byte"], 0);
    assert_eq!(json["file_last_byte"], 0xEF);
}

/// Encode a single named text field as a `multipart/form-data` wire request.
fn encode_multipart_text(boundary: &str, path: &str, field: &str, value: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{field}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_owned(),
        format!("multipart/form-data; boundary={boundary}"),
    );
    let header_json = ::serde_json::json!({
        "v": 1,
        "method": "POST",
        "path": path,
        "headers": headers,
    });
    let header_bytes = ::serde_json::to_vec(&header_json).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(&body);
    wire
}

#[test]
fn string_field_over_default_cap_rejected_413() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // 1 MiB + 1 byte of text in an UNANNOTATED `String` field exceeds the
    // default 1 MiB cap → 413 (FieldTooLarge), instead of buffering the
    // whole payload unbounded.
    let payload = vec![b'x'; 1024 * 1024 + 1];
    let wire = encode_multipart_text("----TextCapBoundary", "/text", "text", &payload);
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, _body) = decode_wire(&resp);
    assert_eq!(
        header["status"].as_u64(),
        Some(413),
        "oversized unannotated String field must be rejected with 413, got header={header:#}"
    );
}

#[test]
fn string_field_under_default_cap_ok() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let payload = vec![b'x'; 1024]; // 1 KiB — well under the 1 MiB cap
    let wire = encode_multipart_text("----TextOkBoundary", "/text", "text", &payload);
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    assert_eq!(header["status"].as_u64(), Some(200), "header={header:#}");
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["text_len"].as_u64(), Some(1024));
}

#[test]
fn typed_multipart_aggregate_total_cap_rejected_413() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let wire = encode_multipart_text(
        "----AggregateTotalBoundary",
        "/text-aggregate-total",
        "text",
        b"ninebytes",
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    assert_eq!(header["status"].as_u64(), Some(413), "header={header:#}");
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["errors"][0]["path"], "text");
}

#[test]
fn typed_multipart_aggregate_field_count_cap_rejected_413() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let wire = encode_multipart_text(
        "----AggregateFieldsBoundary",
        "/text-aggregate-field-count",
        "text",
        b"ok",
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, _body) = decode_wire(&resp);
    assert_eq!(header["status"].as_u64(), Some(413), "header={header:#}");
}

#[test]
fn typed_multipart_unknown_fields_count_toward_max_fields() {
    // Regression: in non-strict mode an UNKNOWN part (the generated `_ => {}`
    // dispatch arm) must still count against `max_fields`.  Before the fix,
    // counting happened only inside the per-known-field parsers, so a flood of
    // unknown parts bypassed the cap entirely (a DoS-adjacent gap) and this
    // request would instead fail later with a 400 missing-field error.  With
    // `MAX_FIELDS = 0`, even one part — known OR unknown — must be rejected 413.
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let wire = encode_multipart_text(
        "----UnknownFieldCountBoundary",
        "/text-aggregate-field-count",
        "definitely_not_a_known_field",
        b"x",
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, _body) = decode_wire(&resp);
    assert_eq!(
        header["status"].as_u64(),
        Some(413),
        "an unknown multipart part must count against max_fields, got header={header:#}"
    );
}

#[test]
fn typed_multipart_aggregate_under_limit_passes() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let wire = encode_multipart_text(
        "----AggregateOkBoundary",
        "/text-aggregate-total",
        "text",
        b"eight888",
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    assert_eq!(header["status"].as_u64(), Some(200), "header={header:#}");
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["text_len"].as_u64(), Some(8));
}

#[test]
fn string_field_unlimited_optout_allows_large() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // 1 MiB + 1 byte through the explicit `#[form_data(limit = "unlimited")]`
    // opt-out must pass — the `Some(usize::MAX)` sentinel keeps the field
    // genuinely unbounded, proving the opt-out survived the cap change.
    let payload = vec![b'y'; 1024 * 1024 + 1];
    let wire = encode_multipart_text(
        "----TextUnlimitedBoundary",
        "/text-unlimited",
        "text",
        &payload,
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    assert_eq!(
        header["status"].as_u64(),
        Some(200),
        "unlimited opt-out must allow >1 MiB text, got header={header:#}"
    );
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["text_len"].as_u64(), Some(1024 * 1024 + 1));
}

#[test]
fn named_temp_file_over_default_cap_rejected_413() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let payload = vec![b'z'; DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES + 1];
    let wire = encode_multipart_upload_wire(
        "/capped-upload",
        "----TempFileCapBoundary",
        "bob",
        "too-large.bin",
        &payload,
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, _body) = decode_wire(&resp);
    assert_eq!(
        header["status"].as_u64(),
        Some(413),
        "oversized unannotated tempfile field must be rejected with 413, got header={header:#}"
    );
}

#[test]
fn validated_typed_multipart_rejects_garde_failure_422() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let wire = encode_multipart_text(
        "----ValidatedMultipartBoundary",
        "/validated-multipart",
        "name",
        b"xy",
    );
    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&resp);
    assert_eq!(
        header["status"].as_u64(),
        Some(422),
        "garde failure must be rejected with 422, got header={header:#}"
    );
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["errors"][0]["path"], "name");
}

#[test]
fn invalid_boolean_returns_422_with_bounded_conversion_message() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let wire = encode_multipart_text(
        "----InvalidBooleanBoundary",
        "/boolean",
        "enabled",
        b"definitely-not-a-boolean",
    );

    let response = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&response);

    assert_eq!(header["status"].as_u64(), Some(422));
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["errors"][0]["path"], "enabled");
    assert!(
        json["errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("invalid boolean value"))
    );
}

#[test]
fn custom_field_parser_observes_metadata_headers_and_whole_bytes() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let wire = encode_multipart_upload_wire(
        "/observed",
        "----ObservedBoundary",
        "ignored",
        "evidence.bin",
        b"proof",
    );

    let response = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&response);

    assert_eq!(header["status"].as_u64(), Some(200));
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["file_name"], "evidence.bin");
    assert_eq!(json["content_type"], "application/octet-stream");
    assert_eq!(json["byte_count"], 5);
    assert_eq!(json["captured_header"], "yes");
}

#[test]
fn custom_field_parser_reads_exactly_at_public_limit() {
    install_router_once();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let wire = encode_multipart_text("----LimitedBoundary", "/limited", "value", b"four");

    let response = dispatch_from_bytes(wire, &runtime);
    let (header, body) = decode_wire(&response);

    assert_eq!(header["status"].as_u64(), Some(200));
    let json: Value = ::serde_json::from_slice(&body).expect("response is JSON");
    assert_eq!(json["text_len"], 4);
}

#[tokio::test]
async fn multipart_rejection_exposes_axum_source_error() {
    let request = Request::builder()
        .method("POST")
        .uri("/text")
        .body(Body::empty())
        .unwrap();

    let Err(error) = TypedMultipart::<TextReq>::from_request(request, &()).await else {
        panic!("missing content type must reject multipart extraction");
    };

    assert!(matches!(error, TypedMultipartError::InvalidRequest { .. }));
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn malformed_multipart_body_exposes_stream_source_error() {
    let request = Request::builder()
        .method("POST")
        .uri("/text")
        .header("content-type", "multipart/form-data; boundary=broken")
        .body(Body::from(
            "--broken\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\nunterminated",
        ))
        .unwrap();

    let Err(error) = TypedMultipart::<TextReq>::from_request(request, &()).await else {
        panic!("unterminated multipart body must reject extraction");
    };

    assert!(matches!(
        error,
        TypedMultipartError::InvalidRequestBody { .. }
    ));
    assert!(std::error::Error::source(&error).is_some());
}
