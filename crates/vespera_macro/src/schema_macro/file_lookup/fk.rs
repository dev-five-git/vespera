//! Foreign-key lookup for SeaORM HasMany relations.

use super::lookup::{find_struct_in_module, is_extern_crate};

/// Whether a written schema path names a crate the compiler links rather than
/// this crate's module tree: a leading `::` (`::entities::user`), or a first
/// segment linked by name (`use entity::user;`, R43-SM2-L2, as `lookup.rs`
/// checks). The segments of such a path would otherwise be read from `src/`
/// as if they were local (R42-SM2-L2).
pub fn names_other_crate(target_schema_path: &str) -> bool {
    let trimmed = target_schema_path.trim_start();
    let first_segment = trimmed.split("::").next().unwrap_or_default().trim();
    trimmed.starts_with("::") || is_extern_crate(first_segment)
}

/// Find the FK columns from the target entity for a `HasMany` relation with `via_rel`.
///
/// When a `HasMany` relation has `via_rel = "TargetUser"`, this function:
/// 1. Looks up the target entity's `Model` (e.g., in `crate::models::notification`,
///    through its own file or the module tree)
/// 2. Finds the field with matching `relation_enum = "TargetUser"`
/// 3. Returns its `from` (the FK column on the target, e.g. "`target_user_id`")
///    and `to` (the referenced column on the source, e.g. "`id`") values;
///    `to` as written — it names a SOURCE column, so the caller normalizes
///    it against the source model's fields
///
/// Returns None if the target model can't be found or parsed, or if no
/// matching relation (with a `from`) exists.
pub fn find_fk_column_from_target_entity(
    target_schema_path: &str,
    via_rel: &str,
) -> Option<(String, Option<String>)> {
    use crate::schema_macro::seaorm::{
        extract_belongs_to_from_field, extract_belongs_to_to_raw, extract_relation_enum,
    };

    if names_other_crate(target_schema_path) {
        return None;
    }

    let layout = crate::schema_macro::crate_layout::crate_layout()?;
    let src_dir = layout.module_dir;

    // e.g., "crate :: models :: notification :: Schema" -> models::notification
    let module_segments: Vec<&str> = target_schema_path
        .split("::")
        .map(|s| s.trim().trim_start_matches("r#"))
        .filter(|s| {
            !s.is_empty() && !matches!(*s, "Schema" | "Entity" | "crate" | "self" | "super")
        })
        .collect();

    let model_def = find_struct_in_module(&src_dir, &module_segments, "Model")?;
    let model = crate::schema_macro::file_cache::parse_struct_cached(&model_def).ok()?;

    // Search through fields for the one with matching relation_enum
    let syn::Fields::Named(fields_named) = &model.fields else {
        return None;
    };
    let field = fields_named.named.iter().find(|field| {
        !crate::schema_macro::cfg_eval::compiled_out(&field.attrs)
            && extract_relation_enum(&field.attrs).as_deref() == Some(via_rel)
    })?;
    let from = extract_belongs_to_from_field(&field.attrs, &model.fields)?;
    Some((from, extract_belongs_to_to_raw(&field.attrs)))
}

#[cfg(test)]
mod tests {
    use crate::schema_macro::file_lookup::find_struct_from_path;

    use tempfile::TempDir;

    /// The `from` column only (the historical return value these cases pin).
    fn find_fk_column_from_target_entity(path: &str, via_rel: &str) -> Option<String> {
        super::find_fk_column_from_target_entity(path, via_rel).map(|(from, _)| from)
    }
    #[test]
    fn test_find_fk_column_from_target_entity_success() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let notification_model = r#"
pub struct Model {
    pub id: i32,
    pub message: String,
    pub target_user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "target_user_id", to = "id", relation_enum = "TargetUser")]
    pub target_user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("notification.rs"), notification_model).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::notification::Schema", "TargetUser");
        assert_eq!(
            result,
            Some("target_user_id".to_string()),
            "Should find FK column 'target_user_id'"
        );
    }
    #[test]
    fn test_find_fk_column_from_target_entity_mod_rs() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models").join("notification");
        std::fs::create_dir_all(&models_dir).unwrap();
        let notification_model = r#"
pub struct Model {
    pub id: i32,
    pub sender_id: i32,
    #[sea_orm(belongs_to = "super::super::user::Entity", from = "sender_id", to = "id", relation_enum = "Sender")]
    pub sender: BelongsTo<super::super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("mod.rs"), notification_model).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::notification::Schema", "Sender");
        assert_eq!(
            result,
            Some("sender_id".to_string()),
            "Should find FK column from mod.rs"
        );
    }
    /// R42-SM2-L2: `::models::notification::Schema` is another crate's module
    /// (the `--extern` spelling), so this crate's `src/models/notification.rs`
    /// that holds the relation is not read for it.
    #[test]
    fn extern_spelled_target_is_not_read_from_this_crate() {
        let temp_dir = TempDir::new().unwrap();
        let models_dir = temp_dir.path().join("src").join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(
            models_dir.join("notification.rs"),
            "pub struct Model { pub id: i32, pub owner_id: i32, #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_id\", to = \"id\", relation_enum = \"Owner\")] pub owner: BelongsTo<super::user::Entity> }",
        )
        .unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        assert_eq!(
            find_fk_column_from_target_entity("crate::models::notification::Schema", "Owner"),
            Some("owner_id".to_string())
        );
        assert_eq!(
            find_fk_column_from_target_entity("::models::notification::Schema", "Owner"),
            None
        );
    }

    /// R43-SM2-L2 (round 43): `use entity::user;` makes `entity::user::Schema` a
    /// path the compiler links by name. This crate's `src/entity/user.rs` (which
    /// holds a relation of the same name) is not read for it, while the local
    /// spelling of the same path is.
    #[test]
    fn use_imported_extern_crate_target_is_not_read_from_this_crate() {
        let temp_dir = TempDir::new().unwrap();
        let entity_dir = temp_dir.path().join("src").join("entity");
        std::fs::create_dir_all(&entity_dir).unwrap();
        std::fs::write(
            entity_dir.join("user.rs"),
            "pub struct Model { pub id: i32, pub owner_id: i32, #[sea_orm(belongs_to = \"super::user::Entity\", from = \"owner_id\", to = \"id\", relation_enum = \"Owner\")] pub owner: BelongsTo<super::user::Entity> }",
        )
        .unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        assert_eq!(
            find_fk_column_from_target_entity("entity::user::Schema", "Owner"),
            Some("owner_id".to_string())
        );
        let _externs = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--extern\nentity=/deps/libentity.rlib",
        );
        assert_eq!(
            find_fk_column_from_target_entity("entity::user::Schema", "Owner"),
            None
        );
    }

    /// R31-SM2-03 (round 31): the relation twin a `cfg_attr(p, cfg(q))` removes
    /// is not the one whose FK column is read, in either declaration order.
    #[rstest::rstest]
    #[case::disabled_twin_first(
        r#"
    #[cfg_attr(all(), cfg(any()))]
    #[sea_orm(belongs_to = "super::user::Entity", from = "owner_a", to = "id", relation_enum = "Owner")]
    pub owner: BelongsTo<super::user::Entity>,
    #[sea_orm(belongs_to = "super::user::Entity", from = "owner_b", to = "id", relation_enum = "Owner")]
    pub owner_active: BelongsTo<super::user::Entity>,
    "#
    )]
    #[case::active_twin_first(
        r#"
    #[sea_orm(belongs_to = "super::user::Entity", from = "owner_b", to = "id", relation_enum = "Owner")]
    pub owner_active: BelongsTo<super::user::Entity>,
    #[cfg_attr(all(), cfg(any()))]
    #[sea_orm(belongs_to = "super::user::Entity", from = "owner_a", to = "id", relation_enum = "Owner")]
    pub owner: BelongsTo<super::user::Entity>,
    "#
    )]
    fn fk_lookup_ignores_a_cfg_attr_disabled_relation_twin(#[case] relations: &str) {
        let temp_dir = TempDir::new().unwrap();
        let models_dir = temp_dir.path().join("src").join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(
            models_dir.join("notification.rs"),
            format!(
                "pub struct Model {{ pub id: i32, pub owner_a: i32, pub owner_b: i32, {relations} }}"
            ),
        )
        .unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        assert_eq!(
            find_fk_column_from_target_entity("crate::models::notification::Schema", "Owner"),
            Some("owner_b".to_string())
        );
    }

    /// A target path without module segments (`crate::Schema`) searches
    /// the crate root: the root's `Model` answers, and a crate without
    /// `src/` has none (the old test, named "empty module segments return
    /// None", passed only for that reason).
    #[test]
    fn test_find_fk_column_from_target_entity_crate_root() {
        let temp_dir = TempDir::new().unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        crate::schema_macro::file_cache::bump_epoch();
        assert!(
            find_fk_column_from_target_entity("crate::Schema", "SomeRelation").is_none(),
            "a crate without src/ has no root Model"
        );
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("lib.rs"),
            r#"pub struct Model { pub id: i32, pub owner_id: i32,
                #[sea_orm(belongs_to = "super::user::Entity", from = "owner_id", to = "Column::Id", relation_enum = "SomeRelation")]
                pub owner: BelongsTo<super::user::Entity> }"#,
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(
            super::find_fk_column_from_target_entity("crate::Schema", "SomeRelation"),
            // `to` as written: it names a column of the SOURCE model.
            Some(("owner_id".to_string(), Some("Column::Id".to_string())))
        );
    }
    #[test]
    fn test_find_fk_column_from_target_entity_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::nonexistent::Schema", "SomeRelation");
        assert!(result.is_none(), "Non-existent file should return None");
    }
    #[test]
    fn test_find_fk_column_from_target_entity_unparseable_file() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(models_dir.join("broken.rs"), "this is not valid rust {{{{").unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::broken::Schema", "SomeRelation");
        assert!(result.is_none(), "Unparseable file should return None");
    }
    #[test]
    fn test_find_fk_column_from_target_entity_no_model_struct() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let content = r"
pub struct SomethingElse {
    pub id: i32,
}
pub enum Status { Active, Inactive }
";
        std::fs::write(models_dir.join("nomodel.rs"), content).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::nomodel::Schema", "SomeRelation");
        assert!(
            result.is_none(),
            "File without Model struct should return None"
        );
    }
    #[test]
    fn test_find_fk_column_from_target_entity_no_matching_relation_enum() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let model = r#"
pub struct Model {
    pub id: i32,
    pub user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "user_id", to = "id", relation_enum = "Author")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("comment.rs"), model).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::comment::Schema", "TargetUser");
        assert!(
            result.is_none(),
            "Non-matching relation_enum should return None"
        );
    }
    #[test]
    fn test_find_fk_column_from_target_entity_tuple_struct() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let model = "pub struct Model(i32, String);";
        std::fs::write(models_dir.join("tuple.rs"), model).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::tuple::Schema", "SomeRelation");
        assert!(result.is_none(), "Tuple struct Model should return None");
    }
    #[test]
    fn test_find_fk_column_from_target_entity_field_no_from_attr() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let model = r#"
pub struct Model {
    pub id: i32,
    pub user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", to = "id", relation_enum = "TargetUser")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("nofrom.rs"), model).unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::nofrom::Schema", "TargetUser");
        assert!(
            result.is_none(),
            "Field without 'from' attribute should return None"
        );
    }
    #[test]
    fn test_find_struct_from_path_qualified_module_path() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(
            models_dir.join("user.rs"),
            "pub struct Model { pub id: i32, pub name: String }",
        )
        .unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = find_struct_from_path(&ty, None);
        assert!(
            result.is_some(),
            "Should find Model struct via qualified path"
        );
        let (metadata, module_path) = result.unwrap();
        assert!(
            metadata.definition.contains("Model"),
            "Definition should contain Model"
        );
        assert_eq!(
            module_path,
            vec!["crate", "models", "user"],
            "Module path should be inferred from type path"
        );
    }
    #[test]
    fn test_find_struct_from_path_mod_rs_variant() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models").join("user");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(
            models_dir.join("mod.rs"),
            "pub struct Model { pub id: i32, pub email: String }",
        )
        .unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = find_struct_from_path(&ty, None);
        assert!(result.is_some(), "Should find Model struct via mod.rs path");
        let (metadata, _) = result.unwrap();
        assert!(
            metadata.definition.contains("email"),
            "Should find the correct Model with email field"
        );
    }
    /// The model file HAS the relation: only the injected unparseable cached
    /// definition makes the lookup fail. The twin module in the same crate
    /// (same text, no injection) resolves the column first, so the `None`
    /// cannot come from a missing file (R21-SM1-01, round 21).
    #[test]
    fn test_find_fk_column_parse_struct_cached_failure() {
        const MODEL: &str = "pub struct Model { pub id: i32, pub user_id: i32, \
            #[sea_orm(belongs_to, relation_enum = \"SomeRelation\", from = \"user_id\", to = \"id\")] \
            pub user: HasOne<super::user::Entity> }";
        let temp_dir = TempDir::new().unwrap();
        let models_dir = temp_dir.path().join("src").join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        // Distinct module names: the FK lookup caches per target path.
        let model_file = models_dir.join("item.rs");
        std::fs::write(&model_file, MODEL).unwrap();
        std::fs::write(models_dir.join("item_twin.rs"), MODEL).unwrap();
        // The manifest dir is cached per epoch: each lookup starts a new one.
        let lookup = |module: &str| {
            let _env_guard =
                crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
            crate::schema_macro::file_cache::bump_epoch();
            let path = format!("crate::models::{module}::Schema");
            find_fk_column_from_target_entity(&path, "SomeRelation")
        };

        assert_eq!(lookup("item_twin").as_deref(), Some("user_id"));

        crate::schema_macro::file_cache::inject_struct_definition_for_test(
            &model_file,
            "Model",
            "not valid rust {{ struct }}",
        );
        assert!(
            lookup("item").is_none(),
            "Should return None when struct definition fails to parse"
        );
    }
}
