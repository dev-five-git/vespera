//! Missing multipart parts follow serde's rules, for `#[derive(Multipart)]`
//! and for `schema_type!(..., multipart)` DTOs (which used to drop the
//! source's serde default / skip keys and `#[try_from_multipart(strict)]`):
//!
//! - a field's `default = "fn"` also applies to `Option` / `Vec` fields;
//! - a container `#[serde(default)]` / `default = "fn"` fills a missing field
//!   from the CONTAINER's default value, not the field type's `Default`;
//! - `skip` / `skip_deserializing` fields are never read.
//!
//! It also pins that a `schema_type!` DTO of a source with
//! `#[serde(default = "fn")]` compiles: the re-rooted `crate::..::fn` path is
//! resolved for the DTO's OpenAPI default instead of being rejected.

#![cfg(feature = "multipart")]

use std::fmt::Write as _;

use vespera::axum::{body::Body, extract::FromRequest, http::Request};
use vespera::multipart::{TryFromMultipartWithState, TypedMultipart};
use vespera::{Multipart, Schema, schema_type};

fn default_tags() -> Vec<String> {
    vec!["fallback".to_string()]
}

#[allow(clippy::unnecessary_wraps)] // serde calls it for an `Option` field
fn default_note() -> Option<String> {
    Some("fallback".to_string())
}

fn seed() -> u32 {
    7
}

#[derive(Multipart, Schema, Debug)]
pub struct FieldDefaults {
    #[serde(default = "default_tags")]
    pub tags: Vec<String>,
    #[serde(default = "default_note")]
    #[schema(default = "fallback")]
    pub note: Option<String>,
}

#[derive(Multipart, Schema, Debug)]
#[serde(default)]
pub struct ContainerDefault {
    pub name: String,
    pub note: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip)]
    pub cache: u8,
    #[serde(default = "seed")]
    pub n: u32,
}

impl Default for ContainerDefault {
    fn default() -> Self {
        Self {
            name: "anon".to_string(),
            note: Some("container".to_string()),
            tags: vec!["container".to_string()],
            cache: 9,
            n: 1,
        }
    }
}

fn make_named() -> ContainerFn {
    ContainerFn {
        name: "from-fn".to_string(),
    }
}

#[derive(Multipart, Schema, Debug)]
#[serde(default = "make_named")]
pub struct ContainerFn {
    pub name: String,
}

/// A `schema_type!` source: its field / container defaults, `skip` and
/// `strict` must reach the multipart DTO.
#[derive(Multipart, Schema, Debug)]
#[serde(default)]
#[try_from_multipart(strict)]
pub struct Source {
    #[serde(default = "default_tags")]
    pub tags: Vec<String>,
    pub title: String,
    #[serde(skip)]
    pub cache: u8,
    #[serde(default = "seed")]
    pub n: u32,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            title: "untitled".to_string(),
            cache: 3,
            n: 2,
        }
    }
}

schema_type!(SourceDto from Source, multipart);
schema_type!(SourcePatch from Source, multipart, partial = ["title", "n"]);
// The serde (non-multipart) DTO of a source with `default = "fn"`.
schema_type!(SourceView from Source, pick = ["tags", "n"]);

async fn parse<T: TryFromMultipartWithState<()>>(fields: &[(&str, &str)]) -> Result<T, String> {
    let boundary = "XBOUNDARY";
    let mut body = String::new();
    for (name, value) in fields {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
        .unwrap();
    }
    write!(body, "--{boundary}--\r\n").unwrap();
    let request = Request::builder()
        .method("POST")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    TypedMultipart::<T>::from_request(request, &())
        .await
        .map(|TypedMultipart(value)| value)
        .map_err(|rejection| format!("{rejection:?}"))
}

#[tokio::test]
async fn field_default_fn_applies_to_option_and_vec() {
    let empty: FieldDefaults = parse(&[]).await.unwrap();
    assert_eq!(empty.tags, ["fallback"]);
    assert_eq!(empty.note.as_deref(), Some("fallback"));

    let sent: FieldDefaults = parse(&[("tags", "a"), ("note", "n")]).await.unwrap();
    assert_eq!(sent.tags, ["a"]);
    assert_eq!(sent.note.as_deref(), Some("n"));
}

#[tokio::test]
async fn container_default_fills_missing_fields_from_the_container_value() {
    let empty: ContainerDefault = parse(&[]).await.unwrap();
    assert_eq!(empty.name, "anon");
    assert_eq!(empty.note.as_deref(), Some("container"));
    assert_eq!(empty.tags, ["container"]);
    assert_eq!(empty.cache, 9);
    // A field default wins over the container's.
    assert_eq!(empty.n, 7);

    let sent: ContainerDefault = parse(&[("name", "x"), ("cache", "1")]).await.unwrap();
    assert_eq!(sent.name, "x");
    assert_eq!(sent.cache, 9, "a skipped field is never read");

    let from_fn: ContainerFn = parse(&[]).await.unwrap();
    assert_eq!(from_fn.name, "from-fn");
}

#[tokio::test]
async fn multipart_schema_type_keeps_the_source_defaults_skip_and_strict() {
    let empty: SourceDto = parse(&[]).await.unwrap();
    assert_eq!(empty.tags, ["fallback"]);
    assert_eq!(empty.title, "untitled");
    assert_eq!(empty.cache, 3);
    assert_eq!(empty.n, 7);

    // `strict`: unknown and skipped fields are rejected.
    assert!(parse::<SourceDto>(&[("other", "x")]).await.is_err());
    assert!(parse::<SourceDto>(&[("cache", "1")]).await.is_err());

    // `partial`: the container default still builds the DTO (wrapped
    // `Some(..)`); a wrapped field's `default = "fn"` (typed `T`) is dropped,
    // so `n` comes from the container default too.
    let patch: SourcePatch = parse(&[]).await.unwrap();
    assert_eq!(patch.title.as_deref(), Some("untitled"));
    assert_eq!(patch.n, Some(2));
    assert_eq!(patch.tags, ["fallback"]);
}

#[test]
fn serde_schema_type_of_a_default_fn_source_compiles_and_defaults() {
    let view: SourceView = serde_json::from_str("{}").unwrap();
    assert_eq!(view.tags, ["fallback"]);
    assert_eq!(view.n, 7);
}
