use super::*;
use axum::http::StatusCode;
use axum::response::IntoResponse;

#[test]
fn test_str_to_bool_truthy() {
    for val in &[
        "true", "True", "TRUE", "yes", "Yes", "y", "Y", "1", "on", "ON",
    ] {
        assert_eq!(str_to_bool(val), Some(true), "expected true for `{val}`");
    }
}

#[test]
fn test_str_to_bool_falsy() {
    for val in &[
        "false", "False", "FALSE", "no", "No", "n", "N", "0", "off", "OFF",
    ] {
        assert_eq!(str_to_bool(val), Some(false), "expected false for `{val}`");
    }
}

#[test]
fn test_str_to_bool_invalid() {
    for val in &["maybe", "2", "", "yep", "nah"] {
        assert_eq!(str_to_bool(val), None, "expected None for `{val}`");
    }
}

#[test]
fn test_str_to_bool_trims_surrounding_whitespace() {
    // Multipart text values can arrive with incidental surrounding whitespace
    // (e.g. a trailing newline from a client); bool must tolerate it exactly as
    // the numeric field impls do (`text.trim().parse()`), so a padded token
    // parses like the bare token instead of being rejected.
    assert_eq!(str_to_bool("  true  "), Some(true));
    assert_eq!(str_to_bool("true\n"), Some(true));
    assert_eq!(str_to_bool("\tyes\r\n"), Some(true));
    assert_eq!(str_to_bool(" false"), Some(false));
    assert_eq!(str_to_bool("off\n"), Some(false));
    // Trim only touches the ends — internal whitespace stays invalid, and a
    // whitespace-only value is still `None`.
    assert_eq!(str_to_bool("tr ue"), None);
    assert_eq!(str_to_bool("   "), None);
}

#[test]
fn field_metadata_full_headers_are_optional_by_default() {
    let metadata = FieldMetadata {
        name: Some("file".to_owned()),
        file_name: Some("data.bin".to_owned()),
        content_type: Some("application/octet-stream".to_owned()),
        headers: None,
    };

    assert!(metadata.headers().is_none());
}

#[test]
fn temp_file_default_limit_is_bounded_and_configurable() {
    assert_eq!(
        default_temp_file_field_limit_bytes(),
        DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES
    );
    assert_eq!(DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES, 16 * 1024 * 1024);

    let previous = set_default_temp_file_field_limit_bytes(2 * 1024 * 1024);
    assert_eq!(previous, DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES);
    assert_eq!(default_temp_file_field_limit_bytes(), 2 * 1024 * 1024);

    let restored = set_default_temp_file_field_limit_bytes(previous);
    assert_eq!(restored, 2 * 1024 * 1024);
    assert_eq!(
        default_temp_file_field_limit_bytes(),
        DEFAULT_TEMP_FILE_FIELD_LIMIT_BYTES
    );
}

#[test]
fn register_multipart_bytes_lets_custom_parsers_enforce_aggregate_cap() {
    // A custom `TryFromFieldWithState` impl that consumes a field's bytes itself
    // can now call the public `register_multipart_bytes` to participate in the
    // request-wide `max_total_bytes` cap — previously impossible (the counter was
    // private), so a single custom-parsed field could read unboundedly past the
    // configured `MultipartLimits`.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");
    let outcome = rt.block_on(async {
        let limits = MultipartLimits::new(10, DEFAULT_MULTIPART_MAX_FIELDS);
        MULTIPART_AGGREGATE
            .scope(RefCell::new(MultipartAggregateState::new(limits)), async {
                // Under the cap: two 4-byte chunks accepted (8 <= 10).
                register_multipart_bytes("custom", 4)?;
                register_multipart_bytes("custom", 4)?;
                // Crossing the cap (8 + 4 = 12 > 10) trips RequestTooLarge.
                register_multipart_bytes("custom", 4)
            })
            .await
    });
    assert!(
        matches!(
            outcome,
            Err(TypedMultipartError::RequestTooLarge {
                limit_bytes: 10,
                ..
            })
        ),
        "custom-parser byte accounting must trip the aggregate cap, got {outcome:?}"
    );

    // Cooperative contract (mirrors `register_multipart_part`): outside the
    // extractor's task-local scope it no-ops rather than erroring, so a derived
    // parser can be unit-tested without a live request aggregate.
    assert!(register_multipart_bytes("custom", usize::MAX).is_ok());
}

// ─── Display tests for all error variants ───────────────────────────

#[test]
fn test_error_display() {
    let err = TypedMultipartError::MissingField {
        field_name: "name".to_string(),
    };
    assert_eq!(err.to_string(), "Missing field: `name`");

    let err = TypedMultipartError::FieldTooLarge {
        field_name: "file".to_string(),
        limit_bytes: 1024,
    };
    assert_eq!(
        err.to_string(),
        "Field `file` exceeds size limit of 1024 bytes"
    );

    let err = TypedMultipartError::WrongFieldType {
        field_name: "age".to_string(),
        wanted: Cow::Borrowed("i32"),
        source: "invalid digit".to_string(),
    };
    assert_eq!(
        err.to_string(),
        "Wrong type for field `age` (expected i32): invalid digit"
    );
}

#[test]
fn test_error_display_duplicate_field() {
    let err = TypedMultipartError::DuplicateField {
        field_name: "email".to_string(),
    };
    assert_eq!(err.to_string(), "Duplicate field: `email`");
}

#[test]
fn other_error_body_hides_internal_source() {
    // The internal source (e.g. a temp-file path / OS error) must NOT
    // leak into the public 500 response body — assert on the ACTUAL
    // serialized envelope (the production path), not an intermediate.
    let err = TypedMultipartError::Other {
        source: "/tmp/vespera-upload-7f3a.part: No such file or directory".to_string(),
    };
    let body = String::from_utf8(err.error_body()).expect("envelope is UTF-8");
    assert_eq!(
        body,
        r#"{"errors":[{"message":"internal error while processing multipart request","path":""}]}"#
    );
    assert!(
        !body.contains("/tmp/"),
        "internal source path leaked into response body"
    );
    // Display still exposes the source for server-side logging.
    assert!(err.to_string().contains("/tmp/"));
    // Non-Other variants stream their (client-safe) Display message verbatim,
    // byte-identical to the prior `to_string()` path.
    let missing = TypedMultipartError::MissingField {
        field_name: "avatar".to_string(),
    };
    let missing_body = String::from_utf8(missing.error_body()).expect("envelope is UTF-8");
    assert_eq!(
        missing_body,
        r#"{"errors":[{"message":"Missing field: `avatar`","path":"avatar"}]}"#
    );
}

#[test]
fn test_error_display_unknown_field() {
    let err = TypedMultipartError::UnknownField {
        field_name: "foo".to_string(),
    };
    assert_eq!(err.to_string(), "Unknown field: `foo`");
}

#[test]
fn test_error_display_invalid_enum_value() {
    let err = TypedMultipartError::InvalidEnumValue {
        field_name: "status".to_string(),
        value: "maybe".to_string(),
    };
    assert_eq!(
        err.to_string(),
        "Invalid enum value `maybe` for field `status`"
    );
}

#[test]
fn invalid_enum_value_constructor_stores_bounded_value() {
    // A clearly-oversized attacker value (far beyond the cap), so the bounded
    // reflection is unambiguously shorter than the input.  The real security
    // property is the CONSTANT ceiling (`cap + marker`), which holds no matter
    // how huge the input is — a value only marginally over the cap can render a
    // few chars longer than the input once the marker is appended, but it is
    // still bounded, so that constant bound is what we assert.
    let oversized = "가".repeat(MAX_REFLECTED_VALUE_CHARS * 4);
    let err = TypedMultipartError::invalid_enum_value("status".to_string(), &oversized);

    match err {
        TypedMultipartError::InvalidEnumValue { value, .. } => {
            assert!(value.ends_with("... (truncated)"));
            assert!(
                value.chars().count()
                    <= MAX_REFLECTED_VALUE_CHARS + "... (truncated)".chars().count()
            );
            assert!(value.chars().count() < oversized.chars().count());
        }
        _ => panic!("expected InvalidEnumValue"),
    }
}

#[test]
fn invalid_bool_message_reflects_bounded_value() {
    let oversized = "x".repeat(MAX_REFLECTED_VALUE_CHARS + 10);
    let message = format!(
        "invalid boolean value: `{}`",
        truncate_reflected_value(&oversized)
    );

    assert!(message.contains("... (truncated)"));
    assert!(!message.contains(&oversized));
}

#[test]
fn test_error_display_nameless_field() {
    let err = TypedMultipartError::NamelessField;
    assert_eq!(err.to_string(), "Encountered a field without a name");
}

#[test]
fn test_error_display_other() {
    let err = TypedMultipartError::Other {
        source: "something went wrong".to_string(),
    };
    assert_eq!(err.to_string(), "something went wrong");
}

// ─── IntoResponse status code tests ─────────────────────────────────

#[test]
fn test_into_response_duplicate_field() {
    let err = TypedMultipartError::DuplicateField {
        field_name: "x".to_string(),
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_into_response_unknown_field() {
    let err = TypedMultipartError::UnknownField {
        field_name: "x".to_string(),
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_into_response_invalid_enum_value() {
    let err = TypedMultipartError::InvalidEnumValue {
        field_name: "x".to_string(),
        value: "bad".to_string(),
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_into_response_nameless_field() {
    let err = TypedMultipartError::NamelessField;
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_into_response_wrong_field_type() {
    let err = TypedMultipartError::WrongFieldType {
        field_name: "age".to_string(),
        wanted: Cow::Borrowed("i32"),
        source: "err".to_string(),
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn test_into_response_field_too_large() {
    let err = TypedMultipartError::FieldTooLarge {
        field_name: "file".to_string(),
        limit_bytes: 100,
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn test_into_response_other() {
    let err = TypedMultipartError::Other {
        source: "err".to_string(),
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn test_into_response_missing_field() {
    let err = TypedMultipartError::MissingField {
        field_name: "x".to_string(),
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── Error trait ────────────────────────────────────────────────────

#[test]
fn test_error_trait_is_implemented() {
    let err: Box<dyn std::error::Error> = Box::new(TypedMultipartError::Other {
        source: "test".to_string(),
    });
    assert_eq!(err.to_string(), "test");
}

// ─── TypedMultipart Deref / DerefMut ────────────────────────────────

#[test]
fn test_typed_multipart_deref() {
    let tm = TypedMultipart("hello".to_string());
    // Deref: &TypedMultipart<String> → &String
    assert_eq!(&*tm, "hello");
    assert_eq!(tm.len(), 5); // auto-deref to String method
}

#[test]
fn test_typed_multipart_deref_mut() {
    let mut tm = TypedMultipart(vec![1, 2, 3]);
    // DerefMut: &mut TypedMultipart<Vec<i32>> → &mut Vec<i32>
    tm.push(4);
    assert_eq!(&*tm, &[1, 2, 3, 4]);
}
