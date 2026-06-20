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
fn other_error_response_message_hides_internal_source() {
    // The internal source (e.g. a temp-file path / OS error) must NOT
    // leak into the public 500 response message.
    let err = TypedMultipartError::Other {
        source: "/tmp/vespera-upload-7f3a.part: No such file or directory".to_string(),
    };
    assert_eq!(
        err.response_message(),
        "internal error while processing multipart request"
    );
    assert!(
        !err.response_message().contains("/tmp/"),
        "internal source path leaked into response message"
    );
    // Display still exposes the source for server-side logging.
    assert!(err.to_string().contains("/tmp/"));
    // Non-Other variants keep their (client-safe) Display message.
    let missing = TypedMultipartError::MissingField {
        field_name: "avatar".to_string(),
    };
    assert_eq!(missing.response_message(), "Missing field: `avatar`");
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
