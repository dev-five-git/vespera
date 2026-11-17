//! Round-21 fixes: the `from_model` load of a relation reads its source
//! column even when `omit` drops that column from the DTO, and a tuple source
//! is rejected before any shape is read.

use std::collections::HashMap;

use super::*;
use crate::schema_macro::file_lookup::set_test_call_site_module;

/// A crate whose `models/memo.rs` has `owner_id` (`owner_id_vis` decides its
/// visibility) as the FK of the `owner` relation, and whose `user` model
/// exists so the relation target resolves.
fn memo_crate(owner_id_vis: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let models = dir.path().join("src").join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(
        models.join("memo.rs"),
        format!(
            "#[sea_orm(table_name = \"memos\")]\npub struct Model {{\n    pub id: i32,\n    {owner_id_vis}owner_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_id\", to = \"id\", relation_enum = \"Owner\")]\n    pub owner: BelongsTo<super::user::Entity>,\n}}"
        ),
    )
    .unwrap();
    std::fs::write(models.join("user.rs"), "pub struct Model { pub id: i32 }").unwrap();
    dir
}

/// Generates `MemoSchema` from the memo model with `owner_id` omitted, as a
/// call site in `crate::routes`.
fn memo_dto_tokens(owner_id_vis: &str) -> String {
    let dir = memo_crate(owner_id_vis);
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let _call_site = set_test_call_site_module("crate::routes");
    let input: SchemaTypeInput =
        syn::parse_str(r#"MemoSchema from crate::models::memo::Model, omit = ["owner_id"]"#)
            .unwrap();
    let (tokens, _) = generate_schema_type_code(&input, &HashMap::new()).unwrap();
    tokens.to_string()
}

/// R21-SM2-L1: the relation's load reads the private `owner_id` from the
/// source, which the DTO omits, so `from_model` must not be emitted (E0616
/// in the call-site module before).
#[test]
fn omitted_private_fk_of_a_relation_stops_from_model() {
    let output = memo_dto_tokens("");
    assert!(!output.contains("from_model"), "{output}");
}

/// The same DTO with a public `owner_id` is readable, so `from_model` stays.
#[test]
fn omitted_public_fk_of_a_relation_keeps_from_model() {
    let output = memo_dto_tokens("pub ");
    assert!(output.contains("from_model"), "{output}");
}

/// R21-SM2-L2: a tuple source is a compile error (the shape tests no longer
/// feed one through).
#[test]
fn tuple_source_is_rejected() {
    let storage = HashMap::from([(
        "Pair".to_string(),
        StructMetadata::new("Pair".to_string(), "pub struct Pair(pub i32);".to_string()),
    )]);
    let input: SchemaTypeInput = syn::parse_str("PairDto from Pair").unwrap();
    let error = generate_schema_type_code(&input, &storage).unwrap_err();
    assert!(
        error.to_string().contains("tuple or unit struct"),
        "{error}"
    );
}
