//! Foreign-key lookup for SeaORM HasMany relations.

use std::path::Path;

use super::lookup::find_struct_in_module;

/// Find the FK columns from the target entity for a `HasMany` relation with `via_rel`.
///
/// When a `HasMany` relation has `via_rel = "TargetUser"`, this function:
/// 1. Looks up the target entity's `Model` (e.g., in `crate::models::notification`,
///    through its own file or the module tree)
/// 2. Finds the field with matching `relation_enum = "TargetUser"`
/// 3. Returns its `from` (the FK column on the target, e.g. "`target_user_id`")
///    and `to` (the referenced column on the source, e.g. "`id`") values
///
/// Returns None if the target model can't be found or parsed, or if no
/// matching relation (with a `from`) exists.
pub fn find_fk_column_from_target_entity(
    target_schema_path: &str,
    via_rel: &str,
) -> Option<(String, Option<String>)> {
    use crate::schema_macro::seaorm::{
        extract_belongs_to_from_field, extract_belongs_to_to_field, extract_relation_enum,
    };

    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // e.g., "crate :: models :: notification :: Schema" -> models::notification
    let module_segments: Vec<&str> = target_schema_path
        .split("::")
        .map(str::trim)
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
    let field = fields_named
        .named
        .iter()
        .find(|field| extract_relation_enum(&field.attrs).as_deref() == Some(via_rel))?;
    let from = extract_belongs_to_from_field(&field.attrs)?;
    Some((from, extract_belongs_to_to_field(&field.attrs)))
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
    #[test]
    fn test_find_fk_column_from_target_entity_empty_module_segments() {
        let temp_dir = TempDir::new().unwrap();
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result = find_fk_column_from_target_entity("crate::Schema", "SomeRelation");
        assert!(result.is_none(), "Empty module segments should return None");
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
    #[test]
    fn test_find_fk_column_parse_struct_cached_failure() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let model_file = models_dir.join("item.rs");
        std::fs::write(&model_file, "pub struct Model { pub id: i32 }").unwrap();
        crate::schema_macro::file_cache::inject_struct_definition_for_test(
            &model_file,
            "Model",
            "not valid rust {{ struct }}",
        );
        let _env_guard =
            crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        let result =
            find_fk_column_from_target_entity("crate::models::item::Schema", "SomeRelation");
        assert!(
            result.is_none(),
            "Should return None when struct definition fails to parse"
        );
    }
}
