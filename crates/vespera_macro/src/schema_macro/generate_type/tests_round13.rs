//! Round-13 fixes: `#[cfg]` twins, the multipart `Vec` default, fields the
//! call site cannot read, a source container default in serde mode, the
//! DTO field visibility, `cfg_attr` schema items in `ignore` metadata, and
//! a source path through another crate's import.

use std::collections::HashMap;

use super::*;
use crate::schema_macro::file_lookup::set_test_call_site_module;

fn stored(name: &str, definition: &str) -> HashMap<String, StructMetadata> {
    HashMap::from([(
        name.to_string(),
        StructMetadata::new(name.to_string(), definition.to_string()),
    )])
}

fn generate(input: &str, storage: &HashMap<String, StructMetadata>) -> (String, Option<String>) {
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let (tokens, metadata) = generate_schema_type_code(&input, storage).unwrap();
    let file: syn::File = syn::parse2(tokens).unwrap();
    (
        prettyplease::unparse(&file),
        metadata.map(|metadata| {
            let file: syn::File = syn::parse_str(&metadata.definition).unwrap();
            prettyplease::unparse(&file)
        }),
    )
}

/// M1 / M2: `#[cfg]` twins each keep their own `#[cfg]` on the `From`
/// assignment (keyed by name, both got the last twin's), and the helpers
/// a field brings (SeaORM default fns, `partial` codec adapters) carry the
/// field's `#[cfg]` and are named by field index for twins (two
/// `default_Dto_id` were E0428; a cfg-off field's helper was dead code).
#[test]
fn cfg_twins_keep_their_own_cfg_and_helpers() {
    let storage = stored(
        "Source",
        r#"pub struct Source {
            #[cfg(feature = "pg")]
            #[sea_orm(primary_key)]
            pub id: i64,
            #[cfg(not(feature = "pg"))]
            #[sea_orm(primary_key)]
            pub id: i32,
            #[cfg(feature = "x")]
            #[sea_orm(default_value = "5")]
            pub seeded: i32,
            #[cfg(feature = "pg")]
            #[serde(with = "codec")]
            pub at: String,
            #[cfg(not(feature = "pg"))]
            #[serde(with = "codec")]
            pub at: String,
            pub plain: bool,
        }"#,
    );
    let (code, _) = generate(r#"Dto from Source, partial = ["at"]"#, &storage);
    insta::assert_snapshot!("round13_cfg_twins", code);
}

/// M3: a multipart DTO's bare `Vec` fields (no default of their own, no
/// container default) get `#[form_data(default)]`, so its schema does not
/// require them (the Multipart derive reads "no parts" as `[]`).
#[test]
fn multipart_vec_fields_are_not_required() {
    let storage = stored(
        "Up",
        r"pub struct Up {
            pub files: Vec<FieldData<NamedTempFile>>,
            pub tags: std::vec::Vec<String>,
            #[serde(default)]
            pub own: Vec<u8>,
            #[form_data(default)]
            pub kept: Vec<u8>,
            pub opt: Option<Vec<u8>>,
            pub title: String,
        }",
    );
    let (code, _) = generate("UpDto from Up, multipart", &storage);
    insta::assert_snapshot!("round13_multipart_vec_defaults", code);
    // A container default fills them itself.
    let storage = stored(
        "Up",
        "#[serde(default)] pub struct Up { pub files: Vec<String> }",
    );
    let (code, _) = generate("UpDto from Up, multipart", &storage);
    assert!(!code.contains("form_data(default)"), "{code}");
}

/// M5: a field the call site cannot read (private, of another module)
/// leaves the `From` out instead of emitting `source.id` (E0616); from the
/// defining module (or below it) the `From` stays.
#[test]
fn from_needs_readable_fields() {
    let storage = stored(
        "User",
        "pub struct User { id: i32, pub(super) parent: i32, pub name: String }",
    );
    let input = "UserDto from crate::models::user::User";
    for (call_site, from) in [
        ("crate::routes::users", false),
        ("crate::models::user", true),
        ("crate::models::user::dto", true),
    ] {
        let _guard = set_test_call_site_module(call_site);
        let (code, _) = generate(input, &storage);
        assert_eq!(code.contains("impl From<"), from, "{call_site}: {code}");
    }
    // `pub(super)` alone: readable from the parent module's tree.
    let storage = stored("User", "pub struct User { pub(super) parent: i32 }");
    let _guard = set_test_call_site_module("crate::models::post");
    assert!(generate(input, &storage).0.contains("impl From<"));
}

/// M6: a serde-mode DTO keeps the source container `#[serde(default)]` as a
/// DTO-building fn (it was stripped: the source accepted `{}`, the DTO not).
#[test]
fn serde_container_default_is_kept() {
    let storage = stored(
        "Settings",
        "#[serde(default)] pub struct Settings { pub a: i32, pub b: String }",
    );
    let (code, _) = generate(r#"SettingsDto from Settings, partial = ["b"]"#, &storage);
    insta::assert_snapshot!("round13_serde_container_default", code);
}

/// L9: a source-relative restricted visibility becomes `pub(crate)` on the
/// DTO (`pub(in crate::models)` emitted in `crate::routes` is E0742).
#[test]
fn restricted_visibility_is_widened() {
    let storage = stored(
        "Source",
        "pub struct Source { pub(in crate::models) a: i32, pub(super) b: i32, pub(crate) c: i32, d: i32 }",
    );
    let (code, _) = generate("Dto from Source", &storage);
    assert!(code.contains("pub(crate) a: i32"), "{code}");
    assert!(code.contains("pub(crate) b: i32"), "{code}");
    assert!(code.contains("pub(crate) c: i32"), "{code}");
    assert!(code.contains("    d: i32"), "{code}");
}

/// L11: with `ignore` + `name`, a `cfg_attr(pred, schema(..))` whose
/// predicate holds goes to the metadata as a plain `#[schema(..)]` (it was
/// dropped; the metadata reads no `cfg_attr`).
#[test]
fn ignored_metadata_keeps_active_cfg_attr_schema() {
    let storage = stored(
        "Source",
        "pub struct Source { #[cfg_attr(all(), schema(maximum = 3))] pub a: i32 }",
    );
    let (code, metadata) = generate(r#"Dto from Source, ignore, name = "DtoSchema""#, &storage);
    assert!(!code.contains("schema("), "{code}");
    let metadata = metadata.unwrap();
    assert!(metadata.contains("#[schema(maximum = 3)]"), "{metadata}");
}

/// M4: a source path through another crate's import (`use entity::types;`)
/// is an error even when a same-named struct is in the storage (that
/// unrelated local `User` used to be taken silently).
#[test]
fn source_through_external_import_is_an_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod handlers;").unwrap();
    std::fs::write(src.join("handlers.rs"), "use entity::types;").unwrap();
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    super::super::file_cache::bump_epoch();
    let _guard = set_test_call_site_module("crate::handlers");
    let storage = stored("User", "pub struct User { pub id: i32 }");
    let input: SchemaTypeInput = syn::parse_str("UserDto from types::User").unwrap();
    let error = generate_schema_type_code(&input, &storage).unwrap_err();
    assert!(error.to_string().contains("another crate"), "{error}");
}

/// L12: the inline type of a cfg-gated circular relation carries the
/// relation field's `#[cfg]` (twins emitted it twice; a gated relation's
/// type named compiled-out items).
#[test]
fn inline_relation_type_carries_the_field_cfg() {
    let dir = tempfile::TempDir::new().unwrap();
    let models = dir.path().join("src").join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(
        models.join("user.rs"),
        "pub struct Model { pub id: i32, pub memo: BelongsTo<super::memo::Entity> }",
    )
    .unwrap();
    std::fs::write(
        models.join("memo.rs"),
        "pub struct Model { pub id: i32, pub user_id: i32, #[cfg(feature = \"rel\")] pub user: BelongsTo<super::user::Entity> }",
    )
    .unwrap();
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    super::super::file_cache::bump_epoch();
    let input: SchemaTypeInput =
        syn::parse_str("MemoSchema from crate::models::memo::Model").unwrap();
    let (tokens, _) = generate_schema_type_code(&input, &HashMap::new()).unwrap();
    let output = tokens.to_string();
    assert!(
        output.contains(
            "# [cfg (feature = \"rel\")] # [derive (Clone , serde :: Serialize , serde :: Deserialize , vespera :: Schema)] # [serde (rename_all = \"camelCase\")] pub struct MemoSchema_User"
        ),
        "{output}"
    );
}

/// L2 (round 14): a source container `#[serde(default)]` the multipart DTO
/// cannot carry (`add` fields, so no DTO-building fn) leaves its bare `Vec`
/// parts to `#[form_data(default)]` — in the emitted struct and in the
/// metadata the schema is built from — instead of listing them as required.
#[test]
fn multipart_vec_fields_default_when_the_container_default_is_dropped() {
    let storage = stored(
        "Up",
        "#[serde(default)] pub struct Up { pub files: Vec<String>, #[serde(default)] pub own: Vec<u8>, pub title: String }",
    );
    let (code, metadata) = generate(
        r#"UpDto from Up, multipart, name = "UpForm", add = [("extra": String)]"#,
        &storage,
    );
    assert!(!code.contains("__vespera_UpDto_default"), "{code}");
    assert_eq!(code.matches("#[form_data(default)]").count(), 1, "{code}");
    assert!(
        code.contains("#[form_data(default)]\n    pub files: Vec<String>"),
        "{code}"
    );
    let metadata = metadata.unwrap();
    assert!(
        metadata.contains("#[form_data(default)]\n    pub files: Vec<String>"),
        "{metadata}"
    );
}

/// R44-SM2-L1: a `#[cfg]` twin's helpers are keyed by field index (`owner_1`
/// for the twin at index 1), which a real field `owner_1` also takes. The
/// bumped key keeps every generated fn name unique (E0428 before).
#[test]
fn cfg_twin_helpers_do_not_collide_with_a_real_field_name() {
    let storage = stored(
        "Source",
        r#"pub struct Source {
            #[cfg(feature = "pg")]
            #[sea_orm(default_value = "1")]
            pub owner: i32,
            #[cfg(not(feature = "pg"))]
            #[sea_orm(default_value = "2")]
            pub owner: i32,
            #[sea_orm(default_value = "3")]
            pub owner_1: i32,
        }"#,
    );
    let (code, _) = generate("Dto from Source", &storage);
    let file: syn::File = syn::parse_str(&code).unwrap();
    let names: Vec<String> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Fn(function) => Some(function.sig.ident.to_string()),
            _ => None,
        })
        .collect();
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    assert!(
        names.iter().any(|name| name.ends_with("owner_1_")),
        "the twin's helpers take the bumped key: {names:?}"
    );
    assert_eq!(unique.len(), names.len(), "duplicate fn names: {names:?}");
}
