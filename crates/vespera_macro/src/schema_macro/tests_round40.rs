//! Round-40 `schema_type!` fixes: a DTO emitted in another module cannot name
//! a private item of its source module (SM4-01), serde function paths follow
//! the same rule (SM4-01), a `#[serde(transparent)]` source keeps the attribute
//! only for a one-field DTO (SM4-02), and a raw source name in `rename` /
//! `partial` resolves (VAL-1, refuted: the names are stored unraw).

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

/// `models/memo.rs`: a `Model` with a `status` field (`status_field`) and a
/// `name` whose serde default is the private `default_name` fn.
fn memo_model(status_field: &str, enum_decl: &str) -> String {
    format!(
        "#[sea_orm(table_name = \"memos\")]\n\
         pub struct Model {{\n    pub id: i32,\n    {status_field}\n    \
         #[serde(default = \"default_name\")]\n    pub name: String,\n}}\n\
         {enum_decl}\n\
         pub(crate) fn default_name() -> String {{ String::new() }}\n"
    )
}

/// The DTO `dsl` generates for a call site in `call_site`, as its tokens.
fn dto_tokens(dir: &tempfile::TempDir, call_site: &str, dsl: &str) -> syn::Result<String> {
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let _call_site = set_test_call_site_module(call_site);
    let input: SchemaTypeInput = syn::parse_str(dsl)?;
    generate_schema_type_code(&input, &HashMap::new()).map(|(tokens, _)| tokens.to_string())
}

const MEMO_DSL: &str = "MemoSchema from crate::models::memo::Model";

/// SM4-01: a private enum of the source module is `E0603` in a DTO emitted in
/// another module, so the DTO is refused with a spanned error instead.
#[test]
fn private_enum_is_refused_from_a_dto_in_another_module() {
    let dir = crate_with(&[(
        "models/memo.rs",
        &memo_model("pub status: MemoStatus,", "enum MemoStatus { Draft }"),
    )]);
    let tokens = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap();
    assert!(
        tokens.contains("is private to `crate::models::memo`"),
        "{tokens}"
    );
}

/// SM4-01: `pub(crate)` is readable from anywhere, so the same DTO is emitted
/// and names the enum by its module path.
#[test]
fn crate_visible_enum_is_emitted_from_another_module() {
    let dir = crate_with(&[(
        "models/memo.rs",
        &memo_model(
            "pub status: MemoStatus,",
            "pub(crate) enum MemoStatus { Draft }",
        ),
    )]);
    let tokens = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap();
    assert!(!tokens.contains("is private to"), "{tokens}");
    assert!(
        tokens.contains("crate :: models :: memo :: MemoStatus"),
        "{tokens}"
    );
}

/// SM4-01: a DTO emitted in the source's own module reads its private items.
#[test]
fn private_enum_is_emitted_inside_its_own_module() {
    let dir = crate_with(&[(
        "models/memo.rs",
        &memo_model("pub status: MemoStatus,", "enum MemoStatus { Draft }"),
    )]);
    let tokens = dto_tokens(&dir, "crate::models::memo", MEMO_DSL).unwrap();
    assert!(!tokens.contains("is private to"), "{tokens}");
}

/// SM4-01: a written module path to a private item, named from another module,
/// is refused like a bare name.
#[test]
fn private_item_named_by_a_module_path_is_refused() {
    let dir = crate_with(&[(
        "models/memo.rs",
        &memo_model(
            "pub status: crate::models::memo::MemoStatus,",
            "enum MemoStatus { Draft }",
        ),
    )]);
    let tokens = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap();
    assert!(
        tokens.contains("is private to `crate::models::memo`"),
        "{tokens}"
    );
}

/// SM4-01: a private `#[serde(default = "..")]` function of the source module
/// is refused from another module (the DTO's serde attribute would not resolve).
#[test]
fn private_serde_default_fn_is_refused_from_another_module() {
    let model = memo_model("pub status: u8,", "").replace("pub(crate) fn", "fn");
    let dir = crate_with(&[("models/memo.rs", &model)]);
    let err = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap_err();
    assert!(
        err.to_string()
            .contains("serde path `default_name` names an item private"),
        "{err}"
    );
    // Readable from the source module itself.
    assert!(dto_tokens(&dir, "crate::models::memo", MEMO_DSL).is_ok());
}

/// SM4-01: a `pub(crate)` serde function is emitted from another module.
#[test]
fn crate_visible_serde_default_fn_is_emitted_from_another_module() {
    let dir = crate_with(&[("models/memo.rs", &memo_model("pub status: u8,", ""))]);
    let tokens = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap();
    assert!(
        tokens.contains("\"crate::models::memo::default_name\""),
        "{tokens}"
    );
}

/// VAL-1 (round 40, refuted): `rename` / `partial` store the source name
/// unraw, so `r#type` finds the field `type` like `pick` does.
#[test]
fn raw_source_names_of_rename_and_partial_resolve() {
    let dir = crate_with(&[(
        "models/memo.rs",
        "#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n    pub r#type: String,\n}\n",
    )]);
    let dsl = "MemoSchema from crate::models::memo::Model, rename = [(\"r#type\", \"r#kind\")], partial = [\"r#type\"]";
    let tokens = dto_tokens(&dir, "crate::routes", dsl).unwrap();
    assert!(!tokens.contains("does not exist"), "{tokens}");
    assert!(tokens.contains("kind"), "{tokens}");
}

/// SM4-02: a `#[serde(transparent)]` source keeps the attribute only when the
/// DTO has one field; a DTO that `add`s or `omit`s into another count is refused.
#[test]
fn transparent_source_keeps_its_attribute_only_for_one_field() {
    let model = "#[serde(transparent)]\n#[sea_orm(table_name = \"memos\")]\npub struct Model {\n    pub id: i32,\n}\n";
    let dir = crate_with(&[("models/memo.rs", model)]);
    let kept = dto_tokens(&dir, "crate::routes", MEMO_DSL).unwrap();
    assert!(kept.contains("serde (transparent)"), "{kept}");

    let added = dto_tokens(
        &dir,
        "crate::routes",
        "MemoSchema from crate::models::memo::Model, add = [(\"extra\": i32)]",
    );
    assert!(added.unwrap_err().to_string().contains("transparent"));

    let omitted = dto_tokens(
        &dir,
        "crate::routes",
        "MemoSchema from crate::models::memo::Model, omit = [\"id\"]",
    );
    assert!(omitted.unwrap_err().to_string().contains("transparent"));
}

/// DEFERRED-X1 (round 40): `default_expr = "Expr::cust(\"NOW()\")"` is the same
/// SQL default as `default_value = "NOW()"`, so the DTO gets the same serde
/// default function (a required `createdAt` was a 422) and the same schema.
#[test]
fn default_expr_cust_gives_the_default_value_serde_default() {
    let with_value = "#[sea_orm(table_name = \"clocks\")]\npub struct Model {\n    pub id: i32,\n    #[sea_orm(default_value = \"NOW()\")]\n    pub created_at: DateTimeUtc,\n}\n";
    let with_expr = "#[sea_orm(table_name = \"clocks\")]\npub struct Model {\n    pub id: i32,\n    #[sea_orm(default_expr = \"Expr::cust(\\\"NOW()\\\")\")]\n    pub created_at: DateTimeUtc,\n}\n";
    let dsl = "ClockSchema from crate::models::clock::Model";
    let value_dir = crate_with(&[("models/clock.rs", with_value)]);
    let expr_dir = crate_with(&[("models/clock.rs", with_expr)]);
    let from_value = dto_tokens(&value_dir, "crate::routes", dsl).unwrap();
    let from_expr = dto_tokens(&expr_dir, "crate::routes", dsl).unwrap();
    assert!(
        from_expr.contains("serde (default = \"default_"),
        "{from_expr}"
    );
    assert_eq!(from_expr, from_value);
}
