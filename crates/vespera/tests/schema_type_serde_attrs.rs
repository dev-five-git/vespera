//! `schema_type!` must re-emit the source struct's serde attributes so the
//! generated DTO both COMPILES and keeps the source's wire names:
//!
//! - `partial` wraps a field into `Option<T>`, so type-dependent field keys
//!   (`default = "f"`, `with`, `serialize_with` / `deserialize_with`,
//!   `skip_serializing_if = "<fn on T>"`) cannot be copied verbatim.
//! - `rename = [("old", "new")]` renames the Rust field only; the JSON key
//!   stays the source's (explicit `#[serde(rename)]`, else `rename_all`).
//! - Container keys that need impls on the DTO (`from` / `try_from` / `into`
//!   / `default`) are dropped, the rest (`deny_unknown_fields`, `tag`, ...)
//!   kept even when they share an attribute with `rename_all`.
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
fn partial_drops_type_dependent_field_keys() {
    let patch: ProfilePatch = serde_json::from_value(json!({ "bigNumber": 7 })).unwrap();
    assert_eq!(patch.big_number, Some(7));
    assert_eq!(patch.display_name, None);
    assert_eq!(patch.other_number, None);
    assert_eq!(patch.email, None);

    let empty: ProfilePatch = serde_json::from_value(json!({})).unwrap();
    // A partial field the source skipped when empty is skipped when `None`.
    let out = serde_json::to_value(&empty).unwrap();
    assert!(out.get("bio").is_none(), "{out}");

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
fn container_tag_kept_with_single_rename_all() {
    let dto = TaggedDto { some_value: 1 };
    assert_eq!(
        serde_json::to_value(&dto).unwrap(),
        json!({ "kind": "TaggedDto", "some_value": 1 })
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
