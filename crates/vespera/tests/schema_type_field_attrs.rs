//! `schema_type!` carries the source field's `#[schema(..)]`, `#[garde(..)]`,
//! `#[deprecated]`, `#[cfg(..)]` and carriable `#[cfg_attr(..)]` attributes
//! (they used to be dropped: constraints vanished from the DTO's OpenAPI
//! schema and validation, a `cfg`-gated field broke the generated `From`).
#![deny(warnings)]

use garde::Validate;
use serde::{Deserialize, Serialize};
use vespera::{Schema, schema_type};

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Signup {
    #[schema(min_length = 3, max_length = 8)]
    pub name: String,
    #[schema(minimum = 18)]
    #[cfg_attr(all(), schema(maximum = 130))]
    pub age: u8,
    #[schema(default = "legacy")]
    pub nick: String,
    #[cfg(any())]
    pub hidden: NotAType,
}

schema_type!(SignupDto from Signup);
schema_type!(SignupPatch from Signup, partial);

#[derive(Clone, garde::Validate)]
pub struct GardeSource {
    #[garde(length(min = 2))]
    pub title: String,
    #[garde(skip)]
    pub views: u32,
    #[garde(skip)]
    pub untouched: bool,
}

/// `#[deprecated]` is carried, so the DTO's own serde derives warn about it
/// exactly like a hand-written struct would.
#[allow(deprecated)]
mod legacy {
    use vespera::schema_type;

    #[derive(Clone)]
    pub struct Legacy {
        pub id: i32,
        #[deprecated = "use id"]
        pub old_id: i32,
    }

    schema_type!(LegacyDto from Legacy, ignore);
}
use legacy::{Legacy, LegacyDto};

schema_type!(GardeDto from GardeSource, ignore, add = [("extra": u8)]);
schema_type!(GardePartial from GardeSource, ignore, partial, omit = ["untouched"]);

#[cfg(any())]
mod never_compiled {
    pub struct Seeded {
        #[sea_orm(primary_key)]
        pub id: i32,
        #[sea_orm(default_value = "5")]
        pub count: i32,
    }
}

// `ignore` (no `Schema` derive) used to keep the SeaORM-derived
// `#[schema(default = ..)]`, an unknown attribute without the derive.
schema_type!(SeededDto from Seeded, ignore, add = [("note": String)]);

#[test]
#[allow(deprecated)]
fn deprecated_fields_convert() {
    let dto = LegacyDto::from(Legacy { id: 1, old_id: 2 });
    assert_eq!((dto.id, dto.old_id), (1, 2));
}

#[test]
fn schema_constraints_follow_the_field() {
    let source = Signup {
        name: "ab".into(),
        age: 17,
        nick: "n".into(),
    };
    let dto = SignupDto::from(source);
    let report = dto.validate().unwrap_err().to_string();
    assert!(report.contains("name"), "{report}");
    assert!(report.contains("age"), "{report}");

    let ok = SignupDto {
        name: "abcd".into(),
        age: 30,
        nick: String::new(),
    };
    assert!(ok.validate().is_ok());
    assert!(
        SignupDto {
            age: 131,
            ..ok.clone()
        }
        .validate()
        .is_err()
    );

    let patch = SignupPatch {
        name: Some("a".into()),
        age: None,
        nick: None,
    };
    assert!(patch.validate().is_err());
}

#[test]
fn garde_rules_follow_the_field() {
    let dto = GardeDto {
        title: "x".into(),
        views: 0,
        untouched: true,
        extra: 1,
    };
    assert!(dto.validate().is_err());
    let partial = GardePartial {
        title: Some("ok".into()),
        views: None,
    };
    assert!(partial.validate().is_ok());
}

#[test]
fn ignored_schema_keeps_serde_defaults_only() {
    let dto: SeededDto = serde_json::from_str(r#"{"note":"n"}"#).unwrap();
    assert_eq!((dto.id, dto.count, dto.note.as_str()), (0, 5, "n"));
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct SplitCase {
    pub user_name: String,
    pub created_by: String,
}

#[derive(Serialize, Deserialize, Clone, Schema)]
#[serde(rename_all(serialize = "kebab-case"))]
pub struct SerializeOnly {
    pub user_name: String,
}

// `rename_all(serialize, deserialize)` used to collapse to the serialize side.
schema_type!(SplitCaseDto from SplitCase, rename = [("created_by", "author")]);
schema_type!(SerializeOnlyDto from SerializeOnly);

#[test]
fn split_rename_all_keeps_both_sides() {
    let dto: SplitCaseDto = serde_json::from_str(r#"{"user_name":"a","created_by":"b"}"#).unwrap();
    assert_eq!(
        serde_json::to_value(&dto).unwrap(),
        serde_json::json!({ "userName": "a", "createdBy": "b" })
    );
    let dto: SerializeOnlyDto = serde_json::from_str(r#"{"user_name":"a"}"#).unwrap();
    assert_eq!(
        serde_json::to_value(&dto).unwrap(),
        serde_json::json!({ "user-name": "a" })
    );
}
