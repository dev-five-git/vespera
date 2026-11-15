//! A `schema_type!` source declared in an inline `mod` (named bare, from
//! the same module): its field types are re-expressed through that inline
//! module, not the file's top level (which used to fail with `E0412`), and a
//! `#[schema(name = "..")]` source is still found by its Rust name.

mod inner {
    use serde::{Deserialize, Serialize};
    use vespera::{Schema, schema_type};

    #[derive(Schema, Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct Tag {
        pub label: String,
    }

    #[derive(Schema, Serialize, Deserialize, Clone)]
    pub struct Source {
        pub id: i32,
        pub tag: Tag,
    }

    schema_type!(SourceDto from Source, pick = ["tag"]);

    #[derive(Schema, Serialize, Deserialize, Clone)]
    #[schema(name = "RenamedRow")]
    pub struct Renamed {
        pub tag: Tag,
    }

    schema_type!(RenamedDto from Renamed);
}

#[test]
fn inline_module_source_keeps_its_field_types() {
    let source = inner::Source {
        id: 1,
        tag: inner::Tag {
            label: "a".to_string(),
        },
    };
    let dto: inner::SourceDto = source.into();
    assert_eq!(dto.tag.label, "a");

    let renamed: inner::RenamedDto = inner::Renamed {
        tag: inner::Tag {
            label: "b".to_string(),
        },
    }
    .into();
    assert_eq!(renamed.tag.label, "b");
}
