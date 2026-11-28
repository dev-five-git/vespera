//! Round-41 `schema_type!` fixes: a source container's private serde default fn
//! (B2-01) and a related model's private serde path in an inline relation type
//! (B2-02) are refused from another module like a field's; a one-name type
//! reached through an import, a glob or a private module of its source is
//! refused by the same readability rule as a written path (SM3-02, SM4-01); an
//! empty `name` is refused at parse time (SM3-03).

use std::collections::HashMap;

use super::file_lookup::set_test_call_site_module;
use super::generate_type::generate_schema_type_code;
use super::input::SchemaTypeInput;

/// A crate with the given `src` files (`path`, `content`).
fn crate_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for (path, content) in files {
        let file = dir.path().join("src").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
    dir
}

/// The DTO `dsl` generates for a call site in `call_site`, as its tokens.
fn dto_tokens(dir: &tempfile::TempDir, call_site: &str, dsl: &str) -> syn::Result<String> {
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let _call_site = set_test_call_site_module(call_site);
    let input: SchemaTypeInput = syn::parse_str(dsl)?;
    generate_schema_type_code(&input, &HashMap::new()).map(|(tokens, _)| tokens.to_string())
}

/// `models/memo.rs`: a `Model` whose container default is the private
/// `default_memo` fn of its own module.
const MEMO_CONTAINER_DEFAULT: &str = "#[serde(default = \"default_memo\")]\n\
     #[sea_orm(table_name = \"memos\")]\n\
     pub struct Model {\n    pub id: i32,\n}\n\
     fn default_memo() -> Model {\n    Model { id: 0 }\n}\n";

const MEMO_DSL: &str = "MemoSchema from crate::models::memo::Model";

/// B2-01: the source container's private default fn is `E0603` in the default
/// helper of a DTO emitted in another module, so the DTO is refused.
#[test]
fn private_container_default_fn_is_refused_from_another_module() {
    let dir = crate_with(&[("models/memo.rs", MEMO_CONTAINER_DEFAULT)]);
    let err = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap_err();
    assert!(
        err.to_string()
            .contains("serde path `default_memo` names an item private"),
        "{err}"
    );
    // Readable from the source module, where the helper is emitted too.
    assert!(
        dto_tokens(&dir, "crate::models::memo", MEMO_DSL)
            .unwrap()
            .contains("default_memo"),
    );
}

/// B2-01: a DTO that is not built from the source (`add`) drops the container
/// default, so the private fn is never named and nothing is refused.
#[test]
fn private_container_default_fn_is_ignored_when_the_dto_is_not_built() {
    let dir = crate_with(&[("models/memo.rs", MEMO_CONTAINER_DEFAULT)]);
    let dsl = "MemoSchema from crate::models::memo::Model, add = [(\"extra\": i32)]";
    let tokens = dto_tokens(&dir, "crate::routes", dsl).unwrap();
    assert!(!tokens.contains("default_memo"), "{tokens}");
}

/// B2-02 (`HasMany`, picked): the inline type of the related `memos` keeps the
/// memo's `title`, whose private serde default fn is `E0603` at the call site.
#[test]
fn private_serde_default_of_a_picked_has_many_item_is_refused() {
    let dir = crate_with(&[
        (
            "models/memo.rs",
            "#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n    pub user_id: i32,\n    #[serde(default = \"default_title\")]\n    pub title: String,\n}\nfn default_title() -> String {\n    String::new()\n}\n",
        ),
        (
            "models/user.rs",
            "pub struct Model {\n    pub id: i32,\n    pub memos: HasMany<super::memo::Entity>,\n}\n",
        ),
    ]);
    let dsl = r#"UserSchema from crate::models::user::Model, pick = ["id", "memos"]"#;
    let err = dto_tokens(&dir, "crate::routes", dsl).unwrap_err();
    assert!(
        err.to_string()
            .contains("serde path `default_title` names an item private"),
        "{err}"
    );
    assert!(dto_tokens(&dir, "crate::models::memo", dsl).is_ok());
}

/// B2-02 (circular `HasOne` back to the parent): the inline `UserSchema_Memo`
/// keeps the memo's `title` with the same private default fn, so it is refused.
#[test]
fn private_serde_default_of_a_circular_inline_type_is_refused() {
    let dir = crate_with(&[
        (
            "models/memo.rs",
            "#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n    pub user_id: i32,\n    #[serde(default = \"default_title\")]\n    pub title: String,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"user_id\", to = \"id\", relation_enum = \"Author\")]\n    pub user: BelongsTo<super::user::Entity>,\n}\nfn default_title() -> String {\n    String::new()\n}\n",
        ),
        (
            "models/user.rs",
            "pub struct Model {\n    pub id: i32,\n    pub memo: HasOne<super::memo::Entity>,\n}\n",
        ),
    ]);
    let dsl = "UserSchema from crate::models::user::Model";
    let err = dto_tokens(&dir, "crate::routes", dsl).unwrap_err();
    assert!(
        err.to_string()
            .contains("serde path `default_title` names an item private"),
        "{err}"
    );
}

/// B2-04: a `serde(transparent)` source gives a multipart DTO no such
/// attribute, so a multipart DTO with two fields is not refused for it.
#[test]
fn transparent_source_is_not_refused_for_a_multipart_dto() {
    let model = "#[serde(transparent)]\n#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub file: String,\n}\n";
    let dir = crate_with(&[("models/memo.rs", model)]);
    let dsl = "UploadDto from crate::models::memo::Model, multipart, add = [(\"title\": String)]";
    assert!(dto_tokens(&dir, "crate::routes", dsl).is_ok());
}

/// SM3-02: a `pub(super)` enum reached through `use super::memo::MemoStatus;`
/// is refused by the spanned error, as the written path is, instead of an
/// E0603 in the emitted DTO.
#[test]
fn pub_super_enum_reached_through_an_import_is_refused() {
    let dir = crate_with(&[
        (
            "models/memo.rs",
            "#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n}\npub(super) enum MemoStatus {\n    Draft,\n}\n",
        ),
        (
            "models/dto.rs",
            "use super::memo::MemoStatus;\npub struct Model {\n    pub id: i32,\n    pub status: MemoStatus,\n}\n",
        ),
    ]);
    let dsl = "DtoSchema from crate::models::dto::Model";
    let tokens = dto_tokens(&dir, "crate::routes", dsl).unwrap();
    assert!(
        tokens.contains("is private to `crate::models::memo`"),
        "{tokens}"
    );
}

/// SM3-02 (glob): the same `pub(super)` enum reached through `use super::memo::*;`.
#[test]
fn pub_super_enum_reached_through_a_glob_is_refused() {
    let dir = crate_with(&[
        (
            "models/memo.rs",
            "#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n}\npub(super) enum MemoStatus {\n    Draft,\n}\n",
        ),
        (
            "models/dto.rs",
            "use super::memo::*;\npub struct Model {\n    pub id: i32,\n    pub status: MemoStatus,\n}\n",
        ),
    ]);
    let dsl = "DtoSchema from crate::models::dto::Model";
    let error = dto_tokens(&dir, "crate::routes", dsl).unwrap();
    assert!(
        error.contains("is private to `crate::models::memo`"),
        "{error}"
    );
}

/// SM4-01 (import): a `pub` enum of a private `mod enums;` reached through an
/// import is refused, as `enums::Status` written in full is.
#[test]
fn pub_enum_of_a_private_module_reached_through_an_import_is_refused() {
    let dir = crate_with(&[
        ("models.rs", "mod enums;\npub mod user;\n"),
        ("models/enums.rs", "pub enum Status {\n    Active,\n}\n"),
        (
            "models/user.rs",
            "use super::enums::Status;\npub struct Model {\n    pub id: i32,\n    pub status: Status,\n}\n",
        ),
    ]);
    let dsl = "UserDto from crate::models::user::Model";
    let error = dto_tokens(&dir, "crate::routes", dsl).unwrap();
    assert!(error.contains("is private to"), "{error}");
    // The same type named in full is refused the same way.
    let written = crate_with(&[
        ("models.rs", "mod enums;\npub mod user;\n"),
        ("models/enums.rs", "pub enum Status {\n    Active,\n}\n"),
        (
            "models/user.rs",
            "pub struct Model {\n    pub id: i32,\n    pub status: super::enums::Status,\n}\n",
        ),
    ]);
    let written = dto_tokens(&written, "crate::routes", dsl).unwrap();
    assert!(written.contains("is private to"), "{written}");
}

/// SM4-01 (local): a `pub` enum of a private `mod user;` (named by its own
/// module, not by an import) is refused from another module.
#[test]
fn pub_local_of_a_private_module_is_refused() {
    let dir = crate_with(&[
        ("models.rs", "mod user;\n"),
        (
            "models/user.rs",
            "pub enum Status {\n    Active,\n}\npub struct Model {\n    pub id: i32,\n    pub status: Status,\n}\n",
        ),
    ]);
    let dsl = "UserDto from crate::models::user::Model";
    let error = dto_tokens(&dir, "crate::routes", dsl).unwrap();
    assert!(
        error.contains("`Status` is private to `crate::models::user`"),
        "{error}"
    );
    // Readable from the module that holds it.
    assert!(dto_tokens(&dir, "crate::models", dsl).is_ok());
}

/// SM3-03: an empty `name` is no OpenAPI component name, so it is refused at
/// parse time instead of registering the component `""`.
#[test]
fn empty_schema_type_name_is_refused() {
    let Err(error) = syn::parse_str::<SchemaTypeInput>("Dto from User, name = \"\"") else {
        panic!("an empty `name` must be refused");
    };
    assert!(error.to_string().contains("must not be empty"), "{error}");
    assert!(syn::parse_str::<SchemaTypeInput>("Dto from User, name = \"Dto\"").is_ok());
}
