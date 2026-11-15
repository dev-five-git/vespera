//! Round 12 multipart contracts:
//!
//! - a part the struct ignores (unknown / nameless, non-strict mode) is
//!   drained through the meter, so its bytes count against the aggregate
//!   `max_total_bytes` cap (they used to bypass it: only the part count was
//!   bounded, while multer still scanned every byte);
//! - a custom field parser can read the part's headers
//!   ([`MeteredField::headers`]);
//! - [`TypedMultipartWithLimits`] derefs to its payload and can be
//!   [`Validated`] like [`vespera::multipart::TypedMultipart`].
#![cfg(all(feature = "multipart", feature = "validation"))]

use ::axum::{
    body::Body,
    extract::FromRequest,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use ::vespera::multipart::{
    FieldMetadata, MeteredField, TryFromFieldWithState, TypedMultipartError,
    TypedMultipartWithLimits,
};
use ::vespera::{Multipart, Validated};

#[derive(Multipart)]
struct Known {
    text: String,
}

/// One multipart part: optional form name, extra part headers, body bytes.
type Part<'a> = (Option<&'a str>, &'a [(&'a str, &'a str)], &'a [u8]);

fn request(parts: &[Part<'_>]) -> Request<Body> {
    let mut body = Vec::new();
    for (name, headers, bytes) in parts {
        body.extend_from_slice(b"--B\r\nContent-Disposition: form-data");
        if let Some(name) = name {
            body.extend_from_slice(format!("; name=\"{name}\"").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        for (key, value) in *headers {
            body.extend_from_slice(format!("{key}: {value}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--B--\r\n");
    Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "multipart/form-data; boundary=B")
        .body(Body::from(body))
        .unwrap()
}

async fn extract_known<const MAX_TOTAL_BYTES: usize>(
    req: Request<Body>,
) -> Result<String, TypedMultipartError> {
    TypedMultipartWithLimits::<Known, MAX_TOTAL_BYTES, 16>::from_request(req, &())
        .await
        .map(|TypedMultipartWithLimits(known)| known.text)
}

#[tokio::test]
async fn unknown_part_bytes_count_against_the_aggregate_cap() {
    let big = vec![b'x'; 64];
    let req = request(&[(Some("unknown"), &[], &big), (Some("text"), &[], b"ok")]);
    let err = extract_known::<32>(req)
        .await
        .expect_err("64 ignored bytes > 32");
    assert!(
        matches!(err, TypedMultipartError::RequestTooLarge { ref field_name, limit_bytes: 32 } if field_name == "unknown"),
        "{err:?}"
    );

    // Under the cap the ignored part is skipped as before.
    let req = request(&[(Some("unknown"), &[], b"tiny"), (Some("text"), &[], b"ok")]);
    assert_eq!(extract_known::<32>(req).await.unwrap(), "ok");
}

#[tokio::test]
async fn nameless_part_bytes_count_against_the_aggregate_cap() {
    let big = vec![b'x'; 64];
    let req = request(&[(None, &[], &big), (Some("text"), &[], b"ok")]);
    let err = extract_known::<32>(req)
        .await
        .expect_err("64 ignored bytes > 32");
    assert!(
        matches!(
            err,
            TypedMultipartError::RequestTooLarge {
                limit_bytes: 32,
                ..
            }
        ),
        "{err:?}"
    );

    let req = request(&[(None, &[], b"tiny"), (Some("text"), &[], b"ok")]);
    assert_eq!(extract_known::<32>(req).await.unwrap(), "ok");
}

/// Reads a part header the built-in metadata does not capture.
struct ContentId(Option<String>);

impl<S: Send + Sync> TryFromFieldWithState<S> for ContentId {
    async fn try_from_field_with_state(
        field: MeteredField<'_>,
        _limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let metadata = FieldMetadata::from(&field).with_headers(field.headers().clone());
        let id = metadata
            .headers()
            .and_then(|headers| headers.get("content-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        field.bytes().await?;
        Ok(Self(id))
    }
}

#[derive(Multipart)]
struct WithContentId {
    part: ContentId,
}

#[tokio::test]
async fn custom_parser_reads_the_part_headers() {
    let req = request(&[(Some("part"), &[("Content-ID", "<img-1@example>")], b"data")]);
    let TypedMultipartWithLimits(parsed) =
        TypedMultipartWithLimits::<WithContentId, 1024>::from_request(req, &())
            .await
            .unwrap();
    assert_eq!(parsed.part.0.as_deref(), Some("<img-1@example>"));
}

#[derive(Multipart, garde::Validate)]
struct Named {
    #[garde(length(min = 3))]
    name: String,
}

#[tokio::test]
async fn typed_multipart_with_limits_can_be_validated() {
    let req = request(&[(Some("name"), &[], b"ab")]);
    let Err(rejection) =
        Validated::<TypedMultipartWithLimits<Named, 1024, 8>>::from_request(req, &()).await
    else {
        panic!("a 2-char name must fail validation");
    };
    assert_eq!(
        rejection.into_response().status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let req = request(&[(Some("name"), &[], b"alice")]);
    let Ok(Validated(mut extracted)) =
        Validated::<TypedMultipartWithLimits<Named, 1024, 8>>::from_request(req, &()).await
    else {
        panic!("a valid name must pass");
    };
    // `Deref` / `DerefMut` reach the payload, like `TypedMultipart`.
    assert_eq!(extracted.name, "alice");
    extracted.name.push('!');
    let payload: &Named = &extracted;
    assert_eq!(payload.name, "alice!");
}
