//! Round-21 and round-22 fixes: the `from_model` load of a relation (adapters
//! included) reads its source column even when `omit` drops that column from
//! the DTO, a related model's private field is left out of its inline type, and
//! a tuple source is rejected before any shape is read.

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

/// The DTO `dsl` generates in the crate `dir`, as a call site in `crate::routes`.
fn dto_tokens_of(dir: &tempfile::TempDir, dsl: &str) -> String {
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let _call_site = set_test_call_site_module("crate::routes");
    let input: SchemaTypeInput = syn::parse_str(dsl).unwrap();
    let (tokens, _) = generate_schema_type_code(&input, &HashMap::new()).unwrap();
    tokens.to_string()
}

/// Generates `MemoSchema` from the memo model with `owner_id` omitted.
fn memo_dto_tokens(owner_id_vis: &str) -> String {
    let dir = memo_crate(owner_id_vis);
    dto_tokens_of(
        &dir,
        r#"MemoSchema from crate::models::memo::Model, omit = ["owner_id"]"#,
    )
}

/// R22-SM2-M1: an adapter relation is loaded from the source the same way, so
/// its private FK stops `from_model` too (E0616 in the call-site module).
#[test]
fn omitted_private_fk_of_an_adapter_relation_stops_from_model() {
    let dir = memo_crate("");
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let _call_site = set_test_call_site_module("crate::routes");
    // The adapter wrapper is a stored `Schema` of the crate (no same-file item).
    let storage = HashMap::from([(
        "OwnerCard".to_string(),
        StructMetadata::new(
            "OwnerCard".to_string(),
            "pub struct OwnerCard { pub id: i32 }".to_string(),
        ),
    )]);
    let input: SchemaTypeInput = syn::parse_str(
        r#"MemoSchema from crate::models::memo::Model, omit = ["owner_id"], relation_adapters = [("owner", OwnerCard)]"#,
    )
    .unwrap();
    let (tokens, _) = generate_schema_type_code(&input, &storage).unwrap();
    let output = tokens.to_string();
    assert!(!output.contains("from_model"), "{output}");
}

/// A `user` whose `memo` back-reference is circular, so `UserSchema_Memo`
/// copies the memo's plain columns; `secret` is private to `models::memo`.
fn circular_crate() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let models = dir.path().join("src").join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(
        models.join("memo.rs"),
        "#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n    pub user_id: i32,\n    secret: String,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"user_id\", to = \"id\", relation_enum = \"Author\")]\n    pub user: BelongsTo<super::user::Entity>,\n}",
    )
    .unwrap();
    std::fs::write(
        models.join("user.rs"),
        "pub struct Model {\n    pub id: i32,\n    pub memo: HasOne<super::memo::Entity>,\n}",
    )
    .unwrap();
    dir
}

/// R22-SM2-L1: the inline type of the circular `memo` relation leaves out the
/// related model's private `secret`, which its `from_model` construction would
/// read from another module (E0616).
#[test]
fn private_field_of_a_circular_related_model_is_left_out() {
    let dir = circular_crate();
    let output = dto_tokens_of(&dir, "UserSchema from crate::models::user::Model");
    assert!(output.contains("UserSchema_Memo"), "{output}");
    assert!(!output.contains("secret"), "{output}");
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
