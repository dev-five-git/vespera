//! `schema_type!` reads `pick` / `omit` names as `schema!` does (round 27): a
//! name is a Rust identifier first and the serde JSON name otherwise. The
//! `rename` / `partial` / `add` / `relation_adapters` names stay Rust names.
#![deny(warnings)]

mod users {
    #[derive(vespera::Schema, serde::Serialize, serde::Deserialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct User {
        pub display_name: String,
        pub email_address: String,
        pub id: i32,
    }

    // `pick` by the camelCase JSON name selects the Rust identifier it renames.
    vespera::schema_type!(PickByJsonName from User, pick = ["displayName", "id"]);
    // `omit` by JSON name drops the field the same way.
    vespera::schema_type!(OmitByJsonName from User, omit = ["emailAddress"]);
    // The Rust identifier keeps working.
    vespera::schema_type!(PickByRustName from User, pick = ["display_name"]);
    // A Rust identifier wins over a JSON name spelled the same way.
    vespera::schema_type!(OmitByRustName from User, omit = ["id"]);

    // Serialize only: the Deserialize visitor of two fields named `b` on the
    // wire would be an unreachable match arm.
    #[derive(vespera::Schema, serde::Serialize, Clone)]
    pub struct Renamed {
        #[serde(rename = "b")]
        pub a: i32,
        pub b: i32,
    }

    // `b` is the Rust identifier of the second field, not the JSON name of the
    // first: the DTO keeps only `b`.
    vespera::schema_type!(PickRustOverJson from Renamed, pick = ["b"]);

    #[test]
    fn rust_identifier_beats_a_json_name_in_pick() {
        let dto = PickRustOverJson { b: 2 };
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({"b": 2})
        );
    }

    #[test]
    fn pick_by_json_name_keeps_the_named_fields() {
        let dto: PickByJsonName =
            serde_json::from_value(serde_json::json!({"displayName": "ada", "id": 7})).unwrap();
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({"displayName": "ada", "id": 7})
        );
    }

    #[test]
    fn omit_by_json_name_drops_the_named_field() {
        let dto: OmitByJsonName = serde_json::from_value(serde_json::json!({
            "displayName": "ada",
            "id": 7
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({"displayName": "ada", "id": 7})
        );
    }

    #[test]
    fn pick_by_rust_name_keeps_the_field() {
        let dto: PickByRustName =
            serde_json::from_value(serde_json::json!({"displayName": "ada"})).unwrap();
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({"displayName": "ada"})
        );
    }

    #[test]
    fn omit_by_rust_name_drops_the_field() {
        let dto: OmitByRustName = serde_json::from_value(serde_json::json!({
            "displayName": "ada",
            "emailAddress": "ada@example.com"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({"displayName": "ada", "emailAddress": "ada@example.com"})
        );
    }
}
