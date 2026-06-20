use std::collections::HashMap;

use rstest::rstest;
use serial_test::serial;

use super::*;

// ── Test support ─────────────────────────────────────────────────
//
// Every scenario snapshots the FULL generated `impl` (pretty-printed
// Rust) under an explicit name — one reviewable artifact per code
// path instead of fragile `contains` probes.  All cases run
// `#[serial]` inside a temp `CARGO_MANIFEST_DIR` so file-lookup
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
        via_rel: None,
    }
}

fn with_inline(mut info: RelationFieldInfo, type_name: &str, fields: &[&str]) -> RelationFieldInfo {
    info.inline_type_info = Some((
        syn::Ident::new(type_name, proc_macro2::Span::call_site()),
        fields.iter().map(ToString::to_string).collect(),
    ));
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
const MEMO_REQUIRED_CIRCULAR: &str = "pub struct Model {\n    pub id: i32,\n    pub title: String,\n    pub user_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"user_id\")]\n    pub user: BelongsTo<super::user::Entity>,\n}";
const MEMO_CIRCULAR: &str = "pub struct Model {\n    pub id: i32,\n    pub title: String,\n    pub user: BelongsTo<super::user::Entity>,\n}";
const PROFILE_CIRCULAR: &str = "pub struct Model {\n    pub id: i32,\n    pub bio: String,\n    pub user: BelongsTo<super::user::Entity>,\n}";
const PROFILE_PLAIN: &str = "pub struct Model {\n    pub id: i32,\n    pub bio: String,\n}";
const SETTINGS_PLAIN: &str = "pub struct Model {\n    pub id: i32,\n    pub theme: String,\n}";
const ADDRESS_FK: &str = "pub struct Model {\n    pub id: i32,\n    pub street: String,\n    pub city_id: i32,\n    pub city: BelongsTo<super::city::Entity>,\n}";
const TAG_FK: &str = "pub struct Model {\n    pub id: i32,\n    pub name: String,\n    pub category_id: i32,\n    pub category: BelongsTo<super::category::Entity>,\n}";
const NOTIFICATION_TARGET_USER: &str = "pub struct Model {\n    pub id: i32,\n    pub message: String,\n    pub target_user_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"target_user_id\", to = \"id\", relation_enum = \"TargetUser\")]\n    pub target_user: BelongsTo<super::user::Entity>,\n}";
const NOTIFICATION_PLAIN: &str =
    "pub struct Model {\n    pub id: i32,\n    pub message: String,\n}";
const COMMENT_AUTHOR_ENUM: &str = "pub struct Model {\n    pub id: i32,\n    pub content: String,\n    pub author_id: i32,\n    #[sea_orm(belongs_to = \"super::user::Entity\", from = \"author_id\", to = \"id\", relation_enum = \"AuthorComments\")]\n    pub author: BelongsTo<super::user::Entity>,\n}";
const POST_PLAIN: &str = "pub struct Model {\n    pub id: i32,\n    pub title: String,\n}";

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
    let temp_dir = tempfile::TempDir::new().unwrap();
    let models_dir = temp_dir.path().join("src").join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    for (file, source) in models {
        std::fs::write(models_dir.join(file), source).unwrap();
    }

    let original = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: every caller is a #[serial] test.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    let tokens = generate_from_model_with_relations(
        &syn::Ident::new(new_type, proc_macro2::Span::call_site()),
        &syn::parse_str::<Type>(source_type).unwrap(),
        &mappings(rows),
        relations,
        &module.iter().map(ToString::to_string).collect::<Vec<_>>(),
        &HashMap::new(),
    );

    // SAFETY: same as above.
    unsafe {
        match original {
            Some(dir) => std::env::set_var("CARGO_MANIFEST_DIR", dir),
            None => std::env::remove_var("CARGO_MANIFEST_DIR"),
        }
    }

    pretty(&tokens)
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
#[case::enum_has_one_optional_no_fk(
        "enum_has_one_optional_no_fk", &[], "MemoSchema", "Model",
        &[("id", "id", false, false), ("author", "author", false, true)],
        vec![with_enum(
            rel("author", "HasOne", quote! { user::Schema }, true),
            Some("Author"), None, None,
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
#[case::enum_belongs_to_required_no_fk(
        "enum_belongs_to_required_no_fk", &[], "CommentSchema", "Model",
        &[("id", "id", false, false), ("author", "author", false, true)],
        vec![with_enum(
            rel("author", "BelongsTo", quote! { user::Schema }, false),
            Some("Author"), None, None,
        )],
        &["crate", "models", "comment"]
    )]
// File-lookup branches (models on disk).
#[case::parent_stub_required_circular(
        "parent_stub_required_circular",
        &[("memo.rs", MEMO_REQUIRED_CIRCULAR), ("user.rs", USER_PLAIN)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("name", "name", false, false), ("memos", "memos", false, true)],
        vec![rel("memos", "HasMany", quote! { crate::models::memo::Schema }, false)],
        &["crate", "models", "user"]
    )]
#[case::circular_has_one_optional(
        "circular_has_one_optional",
        &[("profile.rs", PROFILE_CIRCULAR)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("profile", "profile", false, true)],
        vec![rel("profile", "HasOne", quote! { crate::models::profile::Schema }, true)],
        &["crate", "models", "user"]
    )]
#[case::circular_has_one_required(
        "circular_has_one_required",
        &[("profile.rs", PROFILE_CIRCULAR)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("profile", "profile", false, true)],
        vec![rel("profile", "HasOne", quote! { crate::models::profile::Schema }, false)],
        &["crate", "models", "user"]
    )]
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
#[case::has_many_circular(
        "has_many_circular",
        &[("memo.rs", MEMO_CIRCULAR)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("memos", "memos", false, true)],
        vec![rel("memos", "HasMany", quote! { crate::models::memo::Schema }, false)],
        &["crate", "models", "user"]
    )]
#[case::has_many_fk_no_circular(
        "has_many_fk_no_circular",
        &[("tag.rs", TAG_FK)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("tags", "tags", false, true)],
        vec![rel("tags", "HasMany", quote! { crate::models::tag::Schema }, false)],
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
#[case::parent_stub_all_relation_types(
        "parent_stub_all_relation_types",
        &[
            ("memo.rs", MEMO_REQUIRED_CIRCULAR),
            ("profile.rs", PROFILE_PLAIN),
            ("settings.rs", SETTINGS_PLAIN),
        ],
        "UserSchema", "crate::models::user::Model",
        &[
            ("id", "id", false, false),
            ("memos", "memos", false, true),
            ("profile", "profile", false, true),
            ("settings", "settings", false, true),
            ("orphan_rel", "orphan_rel", false, true),
        ],
        vec![
            rel("memos", "HasMany", quote! { crate::models::memo::Schema }, false),
            rel("profile", "HasOne", quote! { crate::models::profile::Schema }, true),
            rel("settings", "BelongsTo", quote! { crate::models::settings::Schema }, false),
        ],
        &["crate", "models", "user"]
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
#[case::has_many_via_rel_fk_not_found(
        "has_many_via_rel_fk_not_found",
        &[("notification.rs", NOTIFICATION_PLAIN)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("notifications", "notifications", false, true)],
        vec![with_enum(
            rel("notifications", "HasMany", quote! { crate::models::notification::Schema }, false),
            None, None, Some("NonExistentRelation"),
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
#[case::has_many_enum_fk_not_found(
        "has_many_enum_fk_not_found",
        &[("post.rs", POST_PLAIN)],
        "UserSchema", "crate::models::user::Model",
        &[("id", "id", false, false), ("authored_posts", "authored_posts", false, true)],
        vec![with_enum(
            rel("authored_posts", "HasMany", quote! { crate::models::post::Schema }, false),
            Some("NonExistentRelation"), None, None,
        )],
        &["crate", "models", "user"]
    )]
#[serial]
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
