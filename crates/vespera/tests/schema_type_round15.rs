//! Round-15 `schema_type!` source shapes that used to fail:
//!
//! - H1: a multi-segment field type through a `::`-rooted import
//!   (`use ::std::collections;` + `collections::HashMap<..>`) panicked the
//!   proc macro (an empty path segment);
//! - M1: `#[serde(default = "Self::f")]` (valid on a source that only
//!   derives `Serialize`: serde's `Deserialize` visitor is `Self` there) /
//!   `skip_serializing_if = "Self::f"` were copied verbatim, where `Self` is
//!   the DTO (E0599);
//! - L1: `#[cfg_attr(p, cfg(q))]` was dropped, so a field the compiler
//!   removes from the source was emitted and read by `From` (E0609);
//! - L4: `default = "<Vec<String>>::new"` was "cannot statically determine
//!   the OpenAPI default".
#![deny(warnings)]

mod m {
    use ::std::collections;

    use serde::Serialize;
    use vespera::Schema;

    #[derive(Schema, Serialize, Clone)]
    pub struct Query {
        pub counts: collections::HashMap<String, i32>,
        #[serde(default = "Self::default_limit")]
        pub limit: u32,
        #[serde(default, skip_serializing_if = "Self::is_blank")]
        pub note: String,
        #[serde(default = "<Vec<String>>::new")]
        pub tags: Vec<String>,
        #[cfg_attr(all(), cfg(any()))]
        pub debug_only: i32,
    }

    impl Query {
        pub(crate) fn default_limit() -> u32 {
            20
        }

        pub(crate) fn is_blank(value: &str) -> bool {
            value.trim().is_empty()
        }
    }
}

vespera::schema_type!(QueryDto from crate::m::Query);

#[test]
fn round15_source_shapes_compile_and_keep_their_behaviour() {
    let dto: QueryDto = serde_json::from_str(r#"{"counts":{"a":1}}"#).unwrap();
    assert_eq!(dto.counts["a"], 1);
    assert_eq!(dto.limit, 20);
    assert_eq!(dto.tags, Vec::<String>::new());
    let out = serde_json::to_value(&dto).unwrap();
    assert!(out.get("note").is_none(), "{out}");
    assert!(out.get("debugOnly").is_none(), "{out}");

    let converted = QueryDto::from(m::Query {
        counts: std::collections::HashMap::new(),
        limit: 3,
        note: "n".to_string(),
        tags: vec!["t".to_string()],
    });
    assert_eq!((converted.limit, converted.note.as_str()), (3, "n"));

    let schema = vespera::schema!(QueryDto);
    let properties = schema.properties.as_ref().expect("properties");
    let property = |name: &str| serde_json::to_value(&properties[name]).unwrap();
    assert_eq!(property("limit")["default"], serde_json::json!(20));
    assert_eq!(property("tags")["default"], serde_json::json!([]));
    assert!(!properties.contains_key("debugOnly"));
}
