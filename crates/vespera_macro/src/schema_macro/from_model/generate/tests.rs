use std::collections::HashMap;

use rstest::rstest;

use super::*;

// ── Test support ─────────────────────────────────────────────────
//
// Every scenario snapshots the FULL generated `impl` (pretty-printed
// Rust) under an explicit name — one reviewable artifact per code
// path instead of fragile `contains` probes.  All cases run
// inside a (thread-local) temp `CARGO_MANIFEST_DIR` so file-lookup
// branches are deterministic and isolated.

fn pretty(tokens: &TokenStream) -> String {
    let file: syn::File =
        syn::parse2(tokens.clone()).expect("generated tokens must parse as Rust items");
    prettyplease::unparse(&file)
}

/// `(source_field, target_field, wrapped, is_relation)` mapping row.
type MappingRow = (&'static str, &'static str, bool, bool);

fn mappings(rows: &[MappingRow]) -> Vec<(syn::Ident, syn::Ident, bool, bool)> {
    rows.iter()
        .map(|(source, target, wrapped, is_relation)| {
            (
                syn::Ident::new(source, proc_macro2::Span::call_site()),
                syn::Ident::new(target, proc_macro2::Span::call_site()),
                *wrapped,
                *is_relation,
            )
        })
        .collect()
}

fn rel(
    field_name: &str,
    relation_type: &str,
    schema_path: TokenStream,
    is_optional: bool,
) -> RelationFieldInfo {
    RelationFieldInfo {
        field_name: syn::Ident::new(field_name, proc_macro2::Span::call_site()),
        relation_type: relation_type.to_string(),
        schema_path,
        is_optional,
        inline_type_info: None,
        relation_enum: None,
        fk_column: None,
        fk_optional: is_optional,
        fk_to: None,
        via_rel: None,
        adapter: false,
    }
}

fn with_inline(mut info: RelationFieldInfo, type_name: &str, fields: &[&str]) -> RelationFieldInfo {
    info.inline_type_info = Some((
        syn::Ident::new(type_name, proc_macro2::Span::call_site()),
        fields.iter().map(ToString::to_string).collect(),
    ));
    info
}

fn with_to(mut info: RelationFieldInfo, fk_column: &str, to: &str) -> RelationFieldInfo {
    info.fk_column = Some(fk_column.to_string());
    info.fk_to = Some(to.to_string());
    info
}

fn with_enum(
    mut info: RelationFieldInfo,
    relation_enum: Option<&str>,
    fk_column: Option<&str>,
    via_rel: Option<&str>,
) -> RelationFieldInfo {
    info.relation_enum = relation_enum.map(ToString::to_string);
    info.fk_column = fk_column.map(ToString::to_string);
    info.via_rel = via_rel.map(ToString::to_string);
    info
}

/// Model fixtures written under the temp project''s `src/models/`.
const USER_PLAIN: &str = "pub struct Model {\n    pub id: i32,\n    pub name: String,\n}";
const ADDRESS_FK: &str = "pub struct Model {\n    pub id: i32,\n    pub street: String,\n    pub city_id: i32,\n    pub city: BelongsTo<super::city::Entity>,\n}";
const NOTIFICATION_TARGET_USER: &str = "pub struct Model {\n    pub id: i32,\n    pub message: String,\n    pub target_user_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"target_user_id\", to = \"id\", relation_enum = \"TargetUser\")]\n    pub target_user: BelongsTo<super::user::Entity>,\n}";
const NOTIFICATION_PLAIN: &str =
    "pub struct Model {\n    pub id: i32,\n    pub message: String,\n}";
const COMMENT_AUTHOR_ENUM: &str = "pub struct Model {\n    pub id: i32,\n    pub content: String,\n    pub author_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"author_id\", to = \"id\", relation_enum = \"AuthorComments\")]\n    pub author: BelongsTo<super::user::Entity>,\n}";
const POST_PLAIN: &str = "pub struct Model {\n    pub id: i32,\n    pub title: String,\n}";
const NOTIFICATION_BAD_TO: &str = "pub struct Model {\n    pub id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"target_user_id\", to = \"user id\", relation_enum = \"TargetUser\")]\n    pub target_user: BelongsTo<super::user::Entity>,\n}";
/// FK referencing a non-`id` source column.
const ACCOUNT_BY_CODE: &str = "pub struct Model {\n    pub id: i32,\n    pub owner_code: String,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_code\", to = \"code\", relation_enum = \"Owner\")]\n    pub owner: BelongsTo<super::user::Entity>,\n}";
/// FK relation without `to` (falls back to the source `id`).
const PROFILE_OWNER: &str = "pub struct Model {\n    pub id: i32,\n    pub owner_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_id\", relation_enum = \"Owner\")]\n    pub owner: BelongsTo<super::user::Entity>,\n}";

/// A target whose primary key is `id`, with a unique `code`.
const OWNER_KEYED: &str = "pub struct Model {\n    #[sea_orm(primary_key)]\n    pub id: i32,\n    #[sea_orm(unique)]\n    pub code: String,\n}";

/// Run one scenario inside a temp project and return the pretty
/// impl for snapshotting.
#[allow(clippy::too_many_arguments)]
fn run_scenario(
    models: &[(&str, &str)],
    new_type: &str,
    source_type: &str,
    rows: &[MappingRow],
    relations: &[RelationFieldInfo],
    module: &[&str],
) -> String {
    pretty(
        &try_scenario(models, new_type, source_type, rows, relations, module)
            .expect("scenario must generate"),
    )
}

/// [`run_scenario`] without unwrapping, for the error cases.
fn try_scenario(
    models: &[(&str, &str)],
    new_type: &str,
    source_type: &str,
    rows: &[MappingRow],
    relations: &[RelationFieldInfo],
    module: &[&str],
) -> syn::Result<TokenStream> {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let models_dir = temp_dir.path().join("src").join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    for (file, source) in models {
        std::fs::write(models_dir.join(file), source).unwrap();
    }

    let _env_guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());

    generate_from_model_with_relations(
        &syn::Ident::new(new_type, proc_macro2::Span::call_site()),
        &syn::parse_str::<Type>(source_type).unwrap(),
        &mappings(rows),
        relations,
        &module.iter().map(ToString::to_string).collect::<Vec<_>>(),
        &HashMap::new(),
    )
}

/// Target entity whose `relation_enum = "TargetUser"` relation has a
/// malformed `from` (reached through `via_rel`).
const NOTIFICATION_BAD_FROM: &str = "pub struct Model {\n    pub id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"target user id\", to = \"id\", relation_enum = \"TargetUser\")]\n    pub target_user: BelongsTo<super::user::Entity>,\n}";

// A malformed `#[sea_orm(...)]` identifier value is a spanned error, not a
// `syn::Ident::new` panic.
#[rstest]
#[case::relation_enum_with_space(
    "author", &[],
    with_enum(rel("author", "BelongsTo", quote! { crate::models::user::Schema }, false), Some("Author Comments"), None, None),
    "schema_type!: cannot load relation field `author`: no field of the `Model` behind `crate::models::user::Schema` has `#[sea_orm(relation_enum = \"Author Comments\", from = \"..\")]`; add it there, or `omit` the field"
)]
#[case::required_from_with_space(
    "author", &[],
    with_enum(rel("author", "BelongsTo", quote! { crate::models::user::Schema }, false), Some("Author"), Some("author id"), None),
    "schema_type!: relation field `author` has `#[sea_orm(from = \"author id\")]`, but `author id` is not a valid Rust identifier"
)]
#[case::optional_from_with_dash(
    "author", &[],
    with_enum(rel("author", "HasOne", quote! { crate::models::user::Schema }, true), Some("Author"), Some("author-id"), None),
    "schema_type!: relation field `author` has `#[sea_orm(from = \"author-id\")]`, but `author-id` is not a valid Rust identifier"
)]
#[case::via_rel_target_from(
    "notifications", &[("notification.rs", NOTIFICATION_BAD_FROM)],
    with_enum(rel("notifications", "HasMany", quote! { crate::models::notification::Schema }, false), None, None, Some("TargetUser")),
    "schema_type!: relation field `notifications` resolves its foreign key through `TargetUser` to `#[sea_orm(from = \"target user id\")]` on `crate::models::notification::Schema`, but `target user id` is not a valid Rust identifier"
)]
#[case::via_rel_target_to(
    "notifications", &[("notification.rs", NOTIFICATION_BAD_TO)],
    with_enum(rel("notifications", "HasMany", quote! { crate::models::notification::Schema }, false), None, None, Some("TargetUser")),
    "schema_type!: relation field `notifications` has `#[sea_orm(to = \"user id\")]`, but `user id` is not a valid Rust identifier"
)]
// A named relation whose foreign key cannot be found used to load an empty
// `vec![]` (HasMany) or emit an uncompilable `Relation::X` query (HasOne /
// BelongsTo without `from`).
#[case::has_one_enum_no_fk(
    "author", &[],
    with_enum(rel("author", "HasOne", quote! { crate::models::user::Schema }, true), Some("Author"), None, None),
    "schema_type!: cannot load relation field `author`: no field of the `Model` behind `crate::models::user::Schema` has `#[sea_orm(relation_enum = \"Author\", from = \"..\")]`; add it there, or `omit` the field"
)]
#[case::belongs_to_enum_no_fk(
    "author", &[],
    with_enum(rel("author", "BelongsTo", quote! { crate::models::user::Schema }, false), Some("Author"), None, None),
    "schema_type!: cannot load relation field `author`: no field of the `Model` behind `crate::models::user::Schema` has `#[sea_orm(relation_enum = \"Author\", from = \"..\")]`; add it there, or `omit` the field"
)]
#[case::has_many_via_rel_fk_not_found(
    "notifications", &[("notification.rs", NOTIFICATION_PLAIN)],
    with_enum(rel("notifications", "HasMany", quote! { crate::models::notification::Schema }, false), None, None, Some("NonExistentRelation")),
    "schema_type!: cannot load relation field `notifications`: no field of the `Model` behind `crate::models::notification::Schema` has `#[sea_orm(relation_enum = \"NonExistentRelation\", from = \"..\")]`; add it there, or `omit` the field"
)]
#[case::has_many_enum_fk_not_found(
    "authored_posts", &[("post.rs", POST_PLAIN)],
    with_enum(rel("authored_posts", "HasMany", quote! { crate::models::post::Schema }, false), Some("NonExistentRelation"), None, None),
    "schema_type!: cannot load relation field `authored_posts`: no field of the `Model` behind `crate::models::post::Schema` has `#[sea_orm(relation_enum = \"NonExistentRelation\", from = \"..\")]`; add it there, or `omit` the field"
)]
#[case::composite_from(
    "author", &[],
    with_enum(rel("author", "BelongsTo", quote! { crate::models::user::Schema }, false), None, Some("(author_id, tenant_id)"), None),
    "schema_type!: relation field `author` has a composite key `#[sea_orm(from = \"(author_id, tenant_id)\")]`, which `from_model` cannot load; `omit` the field"
)]
#[case::composite_to(
    "author", &[],
    with_to(rel("author", "BelongsTo", quote! { crate::models::user::Schema }, false), "author_id", "(id, tenant_id)"),
    "schema_type!: relation field `author` has a composite key `#[sea_orm(to = \"(id, tenant_id)\")]`, which `from_model` cannot load; `omit` the field"
)]
#[case::to_column_with_space(
    "author", &[],
    with_to(rel("author", "BelongsTo", quote! { crate::models::user::Schema }, false), "author_id", "author code"),
    "schema_type!: relation field `author` has `#[sea_orm(to = \"author code\")]`, but `author code` is not a valid Rust identifier"
)]
fn malformed_sea_orm_ident_is_an_error(
    #[case] field: &'static str,
    #[case] models: &[(&str, &str)],
    #[case] relation: RelationFieldInfo,
    #[case] expected: &str,
) {
    let err = try_scenario(
        models,
        "UserSchema",
        "crate::models::user::Model",
        &[("id", "id", false, false), (field, field, false, true)],
        &[relation],
        &["crate", "models", "user"],
    )
    .expect_err("malformed identifier must be a syn::Error");
    assert_eq!(err.to_string(), expected);
}

// ── Scenario table ───────────────────────────────────────────────

#[rstest]
// Plain shapes (no on-disk models needed).
#[case::no_relations(
        "no_relations", &[], "SimpleSchema", "Model",
        &[("id", "id", false, false), ("name", "name", false, false)],
        vec![], &["crate"]
    )]
#[case::wrapped_field(
        "wrapped_field", &[], "TestSchema", "Model",
        &[("id", "id", true, false)],
        vec![], &["crate"]
    )]
#[case::has_one_required_simple(
        "has_one_required_simple", &[], "MemoSchema", "Model",
        &[("id", "id", false, false), ("user", "user", false, true)],
        vec![rel("user", "HasOne", quote! { user::Schema }, false)],
        &["crate", "models", "memo"]
    )]
#[case::has_one_optional_simple(
        "has_one_optional_simple", &[], "MemoSchema", "Model",
        &[("id", "id", false, false), ("user", "user", false, true)],
        vec![rel("user", "HasOne", quote! { user::Schema }, true)],
        &["crate", "models", "memo"]
    )]
#[case::has_many_simple(
        "has_many_simple", &[], "UserSchema", "Model",
        &[("id", "id", false, false), ("memos", "memos", false, true)],
        vec![rel("memos", "HasMany", quote! { memo::Schema }, false)],
        &["crate", "models", "user"]
    )]
#[case::belongs_to_optional_simple(
        "belongs_to_optional_simple", &[], "MemoSchema", "Model",
        &[("id", "id", false, false), ("user", "user", false, true)],
        vec![rel("user", "BelongsTo", quote! { user::Schema }, true)],
        &["crate", "models", "memo"]
    )]
#[case::has_one_optional_inline_type(
        "has_one_optional_inline_type", &[], "MemoSchema", "Model",
        &[("id", "id", false, false), ("user", "user", false, true)],
        vec![with_inline(
            rel("user", "HasOne", quote! { user::Schema }, true),
            "MemoSchema_User", &["id", "name"],
        )],
        &["crate", "models", "memo"]
    )]
#[case::has_many_inline_type(
        "has_many_inline_type", &[], "UserSchema", "Model",
        &[("id", "id", false, false), ("memos", "memos", false, true)],
        vec![with_inline(
            rel("memos", "HasMany", quote! { memo::Schema }, false),
            "UserSchema_Memos", &["id", "title"],
        )],
        &["crate", "models", "user"]
    )]
#[case::unknown_relation_type(
        "unknown_relation_type", &[], "TestSchema", "Model",
        &[("id", "id", false, false), ("unknown", "unknown", false, true)],
        vec![rel("unknown", "UnknownType", quote! { some::Schema }, true)],
        &["crate"]
    )]
#[case::unknown_relation_with_inline_type(
        "unknown_relation_with_inline_type", &[], "TestSchema", "Model",
        &[("id", "id", false, false), ("weird", "weird", false, true)],
        vec![with_inline(
            rel("weird", "UnknownRelationType", quote! { some::Schema }, true),
            "TestSchema_Weird", &["id"],
        )],
        &["crate"]
    )]
#[case::relation_field_not_in_mappings(
        "relation_field_not_in_mappings", &[], "TestSchema", "Model",
        &[("id", "id", false, false), ("owner", "different_name", false, true)],
        vec![rel("user", "HasOne", quote! { user::Schema }, true)],
        &["crate"]
    )]
// relation_enum / fk_column branches.
#[case::enum_has_one_optional_with_fk(
        "enum_has_one_optional_with_fk", &[], "MemoSchema", "Model",
        &[("id", "id", false, false), ("target_user", "target_user", false, true)],
        vec![with_enum(
            rel("target_user", "HasOne", quote! { user::Schema }, true),
            Some("TargetUser"), Some("target_user_id"), None,
        )],
        &["crate", "models", "memo"]
    )]
#[case::enum_belongs_to_required_with_fk(
        "enum_belongs_to_required_with_fk", &[], "CommentSchema", "Model",
        &[("id", "id", false, false), ("post", "post", false, true)],
        vec![with_enum(
            rel("post", "BelongsTo", quote! { post::Schema }, false),
            Some("Post"), Some("post_id"), None,
        )],
        &["crate", "models", "comment"]
    )]
// File-lookup branches (models on disk).
#[case::non_circular_has_one_fk_optional(
        "non_circular_has_one_fk_optional",
        &[("address.rs", ADDRESS_FK)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("address", "address", false, true)],
        vec![rel("address", "HasOne", quote! { crate::models::address::Schema }, true)],
        &["crate", "models", "user"]
    )]
#[case::non_circular_has_one_fk_required(
        "non_circular_has_one_fk_required",
        &[("address.rs", ADDRESS_FK)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("address", "address", false, true)],
        vec![rel("address", "HasOne", quote! { crate::models::address::Schema }, false)],
        &["crate", "models", "user"]
    )]
#[case::inline_type_required_belongs_to(
        "inline_type_required_belongs_to",
        &[("user.rs", USER_PLAIN)],
        "MemoSchema", "crate::models::memo::Model",
        &[("id", "id", false, false), ("user", "user", false, true)],
        vec![with_inline(
            rel("user", "BelongsTo", quote! { crate::models::user::Schema }, false),
            "MemoSchema_User", &["id", "name"],
        )],
        &["crate", "models", "memo"]
    )]
#[case::has_many_via_rel_fk_found(
        "has_many_via_rel_fk_found",
        &[("notification.rs", NOTIFICATION_TARGET_USER)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("target_user_notifications", "target_user_notifications", false, true)],
        vec![with_enum(
            rel("target_user_notifications", "HasMany", quote! { crate::models::notification::Schema }, false),
            None, None, Some("TargetUser"),
        )],
        &["crate", "models", "user"]
    )]
#[case::has_many_enum_fk_found(
        "has_many_enum_fk_found",
        &[("comment.rs", COMMENT_AUTHOR_ENUM)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("author_comments", "author_comments", false, true)],
        vec![with_enum(
            rel("author_comments", "HasMany", quote! { crate::models::comment::Schema }, false),
            Some("AuthorComments"), None, None,
        )],
        &["crate", "models", "user"]
    )]
#[case::has_many_via_rel_custom_to(
        "has_many_via_rel_custom_to",
        &[("account.rs", ACCOUNT_BY_CODE)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("accounts", "accounts", false, true)],
        vec![with_enum(
            rel("accounts", "HasMany", quote! { crate::models::account::Schema }, false),
            Some("Accounts"), None, Some("Owner"),
        )],
        &["crate", "models", "user"]
    )]
#[case::has_one_via_rel_fk_on_target(
        "has_one_via_rel_fk_on_target",
        &[("profile.rs", PROFILE_OWNER)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("profile", "profile", false, true)],
        vec![with_enum(
            rel("profile", "HasOne", quote! { crate::models::profile::Schema }, true),
            Some("Profile"), None, Some("Owner"),
        )],
        &["crate", "models", "user"]
    )]
// `to` names the column the FK references: the primary key keeps
// `find_by_id`, any other column (or no model on disk to tell, and not
// `id`) is selected by a filter on it.
#[case::fk_to_primary_key(
        "fk_to_primary_key",
        &[("owner.rs", OWNER_KEYED)],
        "AccountSchema", "crate::models::account::Model",
        &[("id", "id", false, false), ("owner", "owner", false, true)],
        vec![with_to(rel("owner", "BelongsTo", quote! { crate::models::owner::Schema }, false), "owner_id", "id")],
        &["crate", "models", "account"]
    )]
#[case::fk_to_unique_column(
        "fk_to_unique_column",
        &[("owner.rs", OWNER_KEYED)],
        "AccountSchema", "crate::models::account::Model",
        &[("id", "id", false, false), ("owner", "owner", false, true)],
        vec![with_to(rel("owner", "BelongsTo", quote! { crate::models::owner::Schema }, true), "owner_code", "code")],
        &["crate", "models", "account"]
    )]
#[case::fk_to_without_model(
        "fk_to_without_model",
        &[("owner.rs", "pub struct Model { broken")],
        "AccountSchema", "crate::models::account::Model",
        &[("id", "id", false, false), ("owner", "owner", false, true)],
        vec![with_to(rel("owner", "HasOne", quote! { crate::models::owner::Schema }, false), "owner_code", "code")],
        &["crate", "models", "account"]
    )]
// Relation fields named like the parameters: the loaded locals `model` /
// `db` no longer shadow them.
#[case::relations_named_like_parameters(
        "relations_named_like_parameters", &[], "ListingSchema", "Model",
        &[("id", "id", false, false), ("model", "model", false, true), ("db", "db", true, true)],
        vec![
            with_enum(rel("model", "HasOne", quote! { bakery::Schema }, false), None, Some("model_id"), None),
            with_enum(rel("db", "HasOne", quote! { bakery::Schema }, true), None, Some("db_id"), None),
        ],
        &["crate", "models", "listing"]
    )]
// `BelongsTo<Option<..>>` with a required key column: optional value,
// key read without `match`.
#[case::optional_target_required_key(
        "optional_target_required_key", &[], "CakeSchema", "Model",
        &[("id", "id", false, false), ("bakery", "bakery", false, true)],
        vec![{
            let mut info = with_enum(rel("bakery", "BelongsTo", quote! { bakery::Schema }, true), None, Some("bakery_id"), None);
            info.fk_optional = false;
            info
        }],
        &["crate", "models", "cake"]
    )]
fn generate_from_model_scenario_snapshot(
    #[case] snapshot_name: &str,
    #[case] models: &[(&str, &str)],
    #[case] new_type: &str,
    #[case] source_type: &str,
    #[case] rows: &[MappingRow],
    #[case] relations: Vec<RelationFieldInfo>,
    #[case] module: &[&str],
) {
    insta::assert_snapshot!(
        snapshot_name,
        run_scenario(models, new_type, source_type, rows, &relations, module)
    );
}

/// LOW-4: the call into a target `Schema` follows the `schema_type!` that
/// generates it, not the target `Model`'s relations: an `omit`ted FK
/// relation leaves only `From`, a `pick`ed HasMany only `from_model`, and
/// `add` neither (a targeted error instead of E0599 / E0277).
#[rstest]
#[case::fk_relation_omitted(
    "pub struct Model {\n    pub id: i32,\n    pub profile: HasOne<super::profile::Entity>,\n}\nvespera::schema_type!(Schema from Model, omit = [\"profile\"]);",
    "< crate :: models :: user :: Schema as From < _ >> :: from (r)"
)]
#[case::has_many_picked(
    "pub struct Model {\n    pub id: i32,\n    pub memos: HasMany<super::memo::Entity>,\n}\nvespera::schema_type!(Schema from Model, pick = [\"id\", \"memos\"]);",
    "crate :: models :: user :: Schema :: from_model ("
)]
#[case::other_schema_name_falls_back(
    "pub struct Model {\n    pub id: i32,\n    pub memos: HasMany<super::memo::Entity>,\n}\nvespera::schema_type!(Other from Model, pick = [\"id\", \"memos\"]);",
    "< crate :: models :: user :: Schema as From < _ >> :: from (r)"
)]
#[case::added_field(
    "pub struct Model {\n    pub id: i32,\n}\nschema_type!(Schema from Model, add = [(\"extra\": i32)]);",
    "compile_error ! (\"schema_type!: relation field `author` converts its row into `crate::models::user::Schema`, but the `schema_type!` generating it uses `add` or `multipart`"
)]
fn target_schema_call_follows_its_schema_type(#[case] user: &str, #[case] expected: &str) {
    let tokens = try_scenario(
        &[("user.rs", user)],
        "MemoSchema",
        "crate::models::memo::Model",
        &[
            ("id", "id", false, false),
            ("author", "author", false, true),
        ],
        &[with_enum(
            rel(
                "author",
                "BelongsTo",
                quote! { crate::models::user::Schema },
                true,
            ),
            None,
            Some("author_id"),
            None,
        )],
        &["crate", "models", "memo"],
    )
    .unwrap();
    let rendered = tokens.to_string();
    assert!(rendered.contains(expected), "{rendered}");
}
