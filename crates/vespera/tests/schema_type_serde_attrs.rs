//! `schema_type!` must re-emit the source struct's serde attributes so the
//! generated DTO both COMPILES and keeps the source's wire names:
//!
//! - `partial` wraps a field into `Option<T>`, so type-dependent field keys
//!   (`default = "f"`, `skip_serializing_if = "<fn on T>"`) cannot be copied
//!   verbatim, and `with` / `serialize_with` / `deserialize_with` go through
//!   generated `Option<T>` adapters so the field keeps the source's format.
//! - `rename = [("old", "new")]` renames the Rust field only; the JSON key
//!   stays the source's (explicit `#[serde(rename)]`, else `rename_all`).
//! - Container keys that need impls on the DTO (`from` / `try_from` / `into`
//!   / `default`) are dropped, the rest (`deny_unknown_fields`, `tag`, ...)
//!   kept even when they share an attribute with `rename_all`; a `tag` keeps
//!   the source's tag value (the DTO is renamed to the source's serde name).
//!
//! Every struct below used to be a compile error (or emitted the wrong key).

use serde::{Deserialize, Serialize};
use serde_json::json;
use vespera::{Schema, schema_type};

fn default_name() -> String {
    "fallback".to_string()
}

mod as_string {
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S: Serializer>(value: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Profile {
    pub id: i64,
    #[serde(default = "default_name")]
    pub display_name: String,
    #[serde(with = "as_string")]
    pub big_number: i64,
    #[serde(
        serialize_with = "as_string::serialize",
        deserialize_with = "as_string::deserialize"
    )]
    pub other_number: i64,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub bio: String,
    #[serde(rename = "e-mail", default = "default_name")]
    pub email: String,
}

schema_type!(ProfilePatch from Profile, partial);

#[test]
fn partial_keeps_the_source_codec_wire_format() {
    // `with` / `serialize_with` + `deserialize_with` fields travel as strings,
    // exactly like the source struct.
    let source = serde_json::to_value(Profile {
        id: 1,
        display_name: "n".into(),
        big_number: 7,
        other_number: 8,
        bio: String::new(),
        email: "e".into(),
    })
    .unwrap();
    // (the source has no `rename_all`; the DTO defaults to camelCase)
    assert_eq!(source["big_number"], json!("7"));
    assert_eq!(source["other_number"], json!("8"));

    let patch: ProfilePatch =
        serde_json::from_value(json!({ "bigNumber": "7", "otherNumber": "8" })).unwrap();
    assert_eq!(patch.big_number, Some(7));
    assert_eq!(patch.other_number, Some(8));
    assert_eq!(patch.display_name, None);
    assert_eq!(patch.email, None);
    let out = serde_json::to_value(&patch).unwrap();
    assert_eq!(out["bigNumber"], json!("7"));
    assert_eq!(out["otherNumber"], json!("8"));
    let back: ProfilePatch = serde_json::from_value(out).unwrap();
    assert_eq!(back.big_number, Some(7));
    assert_eq!(back.other_number, Some(8));

    // The source format, not serde's default for `i64`.
    assert!(serde_json::from_value::<ProfilePatch>(json!({ "bigNumber": 7 })).is_err());

    // The `From` conversion keeps it too.
    let converted = serde_json::to_value(ProfilePatch::from(Profile {
        id: 1,
        display_name: "n".into(),
        big_number: 9,
        other_number: 10,
        bio: String::new(),
        email: "e".into(),
    }))
    .unwrap();
    assert_eq!(converted["bigNumber"], json!("9"));
    assert_eq!(converted["otherNumber"], json!("10"));

    let empty: ProfilePatch = serde_json::from_value(json!({})).unwrap();
    assert_eq!(empty.big_number, None);
    // Unset codec fields and fields the source skipped when empty are
    // skipped when `None`.
    let out = serde_json::to_value(&empty).unwrap();
    assert!(out.get("bio").is_none(), "{out}");
    assert!(out.get("bigNumber").is_none(), "{out}");
    assert!(out.get("otherNumber").is_none(), "{out}");

    // Field-level rename shares an attribute with `default = "f"` and survives.
    let with_email: ProfilePatch =
        serde_json::from_value(json!({ "e-mail": "a@b.c", "bio": "hi" })).unwrap();
    assert_eq!(with_email.email.as_deref(), Some("a@b.c"));
    assert_eq!(
        serde_json::to_value(&with_email).unwrap()["bio"],
        json!("hi")
    );
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Account {
    pub user_name: String,
    #[serde(rename = "mail")]
    pub email_address: String,
}

schema_type!(AccountDto from Account, rename = [("user_name", "login"), ("email_address", "email")]);

#[test]
fn rename_keeps_the_rename_all_json_key() {
    let dto = AccountDto::from(Account {
        user_name: "neo".into(),
        email_address: "neo@matrix".into(),
    });
    assert_eq!(dto.login, "neo");
    assert_eq!(
        serde_json::to_value(&dto).unwrap(),
        json!({ "userName": "neo", "mail": "neo@matrix" })
    );
}

#[derive(Serialize, Deserialize, Clone, Schema, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    pub dark_mode: bool,
}

schema_type!(SettingsDto from Settings);

#[test]
fn container_default_dropped_but_deny_unknown_fields_kept() {
    let ok: SettingsDto = serde_json::from_value(json!({ "darkMode": true })).unwrap();
    assert!(ok.dark_mode);
    assert!(serde_json::from_value::<SettingsDto>(json!({ "darkMode": true, "x": 1 })).is_err());
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub struct Tagged {
    pub some_value: i32,
}

schema_type!(TaggedDto from Tagged);

#[test]
fn container_tag_kept_with_the_source_tag_value() {
    let dto = TaggedDto { some_value: 1 };
    let expected = json!({ "kind": "Tagged", "some_value": 1 });
    assert_eq!(
        serde_json::to_value(Tagged { some_value: 1 }).unwrap(),
        expected
    );
    assert_eq!(serde_json::to_value(&dto).unwrap(), expected);
    let back: TaggedDto = serde_json::from_value(expected).unwrap();
    assert_eq!(back.some_value, 1);
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(tag = "type", rename = "renamed-wire")]
pub struct TaggedRenamed {
    pub value: i32,
}

schema_type!(TaggedRenamedDto from TaggedRenamed);

#[test]
fn container_tag_keeps_the_source_rename() {
    assert_eq!(
        serde_json::to_value(TaggedRenamedDto { value: 2 }).unwrap(),
        json!({ "type": "renamed-wire", "value": 2 })
    );
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(from = "Wire", into = "Wire")]
pub struct Converted {
    pub value: i32,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Wire {
    pub value: i32,
}

impl From<Wire> for Converted {
    fn from(w: Wire) -> Self {
        Self { value: w.value }
    }
}

impl From<Converted> for Wire {
    fn from(c: Converted) -> Self {
        Self { value: c.value }
    }
}

schema_type!(ConvertedDto from Converted);

#[test]
fn container_from_into_dropped() {
    let dto: ConvertedDto = serde_json::from_value(json!({ "value": 3 })).unwrap();
    assert_eq!(dto.value, 3);
}

/// `multipart` DTOs do not copy serde attributes, so a source
/// `#[serde(rename)]` is pinned as `#[form_data(field_name)]` (the
/// `Multipart` derive's order: `form_data(field_name)` > serde `rename` >
/// `rename_all`), for renamed and kept fields alike.
#[cfg(feature = "multipart")]
mod multipart_names {
    use std::fmt::Write as _;

    use vespera::axum::{body::Body, extract::FromRequest, http::Request};
    use vespera::multipart::TypedMultipart;
    use vespera::{Multipart, Schema, schema_type};

    #[derive(Multipart, Schema)]
    #[allow(dead_code)]
    pub struct Upload {
        #[serde(rename = "doc-title")]
        pub title: String,
        #[serde(rename = "doc-note")]
        pub note: String,
        #[form_data(field_name = "explicit")]
        #[serde(rename = "ignored")]
        pub other: String,
        pub plain_field: String,
    }

    schema_type!(UploadDto from Upload, multipart, rename = [("title", "heading"), ("plain_field", "plain")], ignore);

    async fn parse(fields: &[(&str, &str)]) -> UploadDto {
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
        match TypedMultipart::<UploadDto>::from_request(request, &()).await {
            Ok(TypedMultipart(dto)) => dto,
            Err(err) => panic!("multipart rejected: {err:?}"),
        }
    }

    #[tokio::test]
    async fn multipart_dto_keeps_the_source_form_names() {
        let dto = parse(&[
            ("doc-title", "t"),
            ("doc-note", "n"),
            ("explicit", "o"),
            ("plainField", "p"),
        ])
        .await;
        assert_eq!(dto.heading, "t");
        assert_eq!(dto.note, "n");
        assert_eq!(dto.other, "o");
        assert_eq!(dto.plain, "p");
    }
}
