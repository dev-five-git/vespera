//! Round-17 `multipart` fixes:
//!
//! - M2: a `schema_type!(.., multipart)` DTO used the source's
//!   SERIALIZE-side `rename_all`, while the source's `Multipart` derive
//!   reads the deserialize side;
//! - L1: it dropped a source field's `#[cfg_attr(p, serde(..))]` (rename /
//!   default) and its `#[serde(alias)]`;
//! - L2: `#[serde(deny_unknown_fields)]` closed the schema while the derive
//!   skipped unknown parts (now rejected with `400`, like `strict`).
#![cfg(feature = "multipart")]

use std::fmt::Write as _;

use vespera::Multipart;
use vespera::axum::{
    body::Body,
    extract::FromRequest,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use vespera::multipart::{TryFromMultipartWithState, TypedMultipart};

mod m {
    use vespera::Multipart;

    #[derive(Multipart)]
    #[serde(rename_all(serialize = "SCREAMING_SNAKE_CASE", deserialize = "kebab-case"))]
    pub struct Upload {
        pub user_name: String,
        #[cfg_attr(all(), serde(rename = "file"))]
        pub doc_title: String,
        #[cfg_attr(all(), serde(default))]
        pub note_text: String,
        #[serde(alias = "lbl")]
        pub label_text: String,
    }

    #[derive(Multipart)]
    #[serde(deny_unknown_fields)]
    pub struct Closed {
        pub name: String,
    }
}

vespera::schema_type!(UploadDto from crate::m::Upload, multipart, ignore);
vespera::schema_type!(ClosedDto from crate::m::Closed, multipart, ignore);

/// A `multipart/form-data` request of `(name, value)` parts.
fn request(parts: &[(&str, &str)]) -> Request<Body> {
    let mut body = String::new();
    for (name, value) in parts {
        write!(
            body,
            "--B\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
        .unwrap();
    }
    body.push_str("--B--\r\n");
    Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "multipart/form-data; boundary=B")
        .body(Body::from(body))
        .unwrap()
}

async fn extract<T>(parts: &[(&str, &str)]) -> Result<T, StatusCode>
where
    T: TryFromMultipartWithState<()>,
{
    TypedMultipart::<T>::from_request(request(parts), &())
        .await
        .map(|TypedMultipart(value)| value)
        .map_err(|rejection| rejection.into_response().status())
}

#[tokio::test]
async fn dto_reads_the_sources_form_field_names() {
    let parts = [("user-name", "u"), ("file", "t"), ("lbl", "l")];
    let source = extract::<m::Upload>(&parts).await.unwrap();
    let dto = extract::<UploadDto>(&parts).await.unwrap();
    assert_eq!(
        (dto.user_name, dto.doc_title, dto.note_text, dto.label_text),
        (
            source.user_name,
            source.doc_title,
            source.note_text,
            source.label_text
        )
    );
    // The serialize-side name is not the form's.
    let screaming = [("USER_NAME", "u"), ("file", "t"), ("label-text", "l")];
    assert!(extract::<UploadDto>(&screaming).await.is_err());
}

#[tokio::test]
async fn deny_unknown_fields_rejects_unknown_parts() {
    let known = [("name", "n")];
    assert_eq!(extract::<m::Closed>(&known).await.unwrap().name, "n");
    assert_eq!(extract::<ClosedDto>(&known).await.unwrap().name, "n");
    let unknown = [("name", "n"), ("extra", "x")];
    let bad = Some(StatusCode::BAD_REQUEST);
    assert_eq!(extract::<m::Closed>(&unknown).await.err(), bad);
    assert_eq!(extract::<ClosedDto>(&unknown).await.err(), bad);
}

#[derive(Multipart)]
struct Open {
    name: String,
}

#[tokio::test]
async fn without_it_unknown_parts_are_skipped() {
    let open = extract::<Open>(&[("name", "n"), ("extra", "x")])
        .await
        .unwrap();
    assert_eq!(open.name, "n");
}
