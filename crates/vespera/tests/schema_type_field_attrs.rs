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
    #[cfg_attr(all(), serde(alias = "years"))]
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

/// `#[deprecated]` is carried; the DTO allows it on its own derives, so
/// using the source (here, with `deny(warnings)`) compiles cleanly. The
/// `Schema`-deriving `partial` twin exercises the other derives.
mod legacy {
    use vespera::schema_type;

    #[derive(Clone)]
    pub struct Legacy {
        pub id: i32,
        #[deprecated = "use id"]
        pub old_id: i32,
    }

    schema_type!(LegacyDto from Legacy, ignore);
    schema_type!(LegacyPatch from Legacy, partial);
}
use legacy::{Legacy, LegacyDto};

schema_type!(GardeDto from GardeSource, ignore, add = [("extra": u8)]);
schema_type!(GardePartial from GardeSource, ignore, partial, omit = ["untouched"]);

// `Seeded` (SeaORM `#[sea_orm(..)]` attributes, which no dev-dependency
// derive accepts) lives in `schema_type_fixtures/seeded.rs`: a file the
// macro reads by module path but cargo never compiles (not a `tests/*.rs`
// target, no `mod` declares it). `add` disables the generated `From`, so
// nothing references the uncompiled type.
//
// `ignore` (no `Schema` derive) used to keep the SeaORM-derived
// `#[schema(default = ..)]`, an unknown attribute without the derive.
schema_type!(SeededDto from crate::schema_type_fixtures::seeded::Seeded, ignore, add = [("note": String)]);
// A raw type name went verbatim into the default fn name (`Ident::new` panic).
schema_type!(r#RawSeeded from crate::schema_type_fixtures::seeded::Seeded, ignore, add = [("note": String)]);

#[test]
#[allow(deprecated)]
fn deprecated_fields_convert() {
    let dto = LegacyDto::from(Legacy { id: 1, old_id: 2 });
    assert_eq!((dto.id, dto.old_id), (1, 2));
}

/// The derived `Validate` follows `#[schema]` constraints (`validation` feature).
#[cfg(feature = "validation")]
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
    // The carried `length(min = 2)` still runs on the `partial` (`Option`) twin.
    assert!(
        GardePartial {
            title: Some("x".into()),
            views: None,
        }
        .validate()
        .is_err()
    );
}

#[test]
fn ignored_schema_keeps_serde_defaults_only() {
    let dto: SeededDto = serde_json::from_str(r#"{"note":"n"}"#).unwrap();
    assert_eq!((dto.id, dto.count, dto.note.as_str()), (0, 5, "n"));
    let raw: RawSeeded = serde_json::from_str(r#"{"note":""}"#).unwrap();
    assert_eq!((raw.id, raw.count), (0, 5));
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

/// A `serde` item inside `cfg_attr` is carried (it used to be dropped).
#[test]
fn cfg_attr_serde_is_carried() {
    let dto: SignupDto = serde_json::from_str(r#"{"name":"abc","years":40,"nick":"n"}"#).unwrap();
    assert_eq!(dto.age, 40);
    let patch: SignupPatch = serde_json::from_str(r#"{"years":41}"#).unwrap();
    assert_eq!(patch.age, Some(41));
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(bound = "T: Serialize + serde::de::DeserializeOwned")]
pub struct Bounded<T> {
    #[serde(bound = "T: Serialize + serde::de::DeserializeOwned")]
    pub value: T,
}

// The source's `bound`s name `T`, which the instantiated DTO lacks: they
// used to be copied onto it (`cannot find type T`).
schema_type!(BoundedInt from Bounded<i32>);

#[test]
fn bounds_of_a_generic_source_are_dropped() {
    let dto: BoundedInt = serde_json::from_str(r#"{"value":3}"#).unwrap();
    assert_eq!(BoundedInt::from(Bounded { value: 3 }).value, dto.value);
}
