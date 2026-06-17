//! Foreign-key lookup for SeaORM HasMany relations.

use std::path::Path;

use super::lookup::candidate_file_paths;

/// Find the FK column name from the target entity for a `HasMany` relation with `via_rel`.
///
/// When a `HasMany` relation has `via_rel = "TargetUser"`, this function:
/// 1. Looks up the target entity file (e.g., notification.rs from schema path)
/// 2. Finds the field with matching `relation_enum = "TargetUser"`
/// 3. Extracts and returns the `from` attribute value (e.g., "`target_user_id`")
///
/// Returns None if the target file can't be found or parsed, or if no matching relation exists.
#[allow(clippy::too_many_lines)]
pub fn find_fk_column_from_target_entity(
    target_schema_path: &str,
    via_rel: &str,
) -> Option<String> {
    use crate::schema_macro::seaorm::{extract_belongs_to_from_field, extract_relation_enum};

    // Get CARGO_MANIFEST_DIR to locate src folder (cached to avoid repeated syscalls)
    let manifest_dir = crate::schema_macro::file_cache::get_manifest_dir()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the schema path to get file path
    // e.g., "crate :: models :: notification :: Schema" -> src/models/notification.rs
    let segments: Vec<&str> = target_schema_path
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "Schema" && *s != "Entity")
        .collect();

    let module_segments: Vec<&str> = segments
        .iter()
        .filter(|s| **s != "crate" && **s != "self" && **s != "super")
        .copied()
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = candidate_file_paths(&src_dir, &module_segments);

    for file_path in file_paths {
        // No `exists()` preflight: `get_struct_definition` returns `None` for
        // a missing/unreadable file via its mtime-validated cache, so the
        // stat is redundant (and TOCTOU-prone).
        let Some(model_def) =
            crate::schema_macro::file_cache::get_struct_definition(&file_path, "Model")
        else {
            continue;
        };
        let Ok(model) = crate::schema_macro::file_cache::parse_struct_cached(&model_def) else {
            continue;
        };

        // Search through fields for the one with matching relation_enum
        if let syn::Fields::Named(fields_named) = &model.fields {
            for field in &fields_named.named {
                let field_relation_enum = extract_relation_enum(&field.attrs);
                if field_relation_enum.as_deref() == Some(via_rel) {
                    // Found the matching field, extract FK column from `from` attribute
                    return extract_belongs_to_from_field(&field.attrs);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use crate::schema_macro::file_lookup::{
        find_struct_by_name_in_all_files, find_struct_from_path,
    };

    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::notification::Schema", "TargetUser");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert_eq!(
            result,
            Some("target_user_id".to_string()),
            "Should find FK column 'target_user_id'"
        );
    }
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::notification::Schema", "Sender");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert_eq!(
            result,
            Some("sender_id".to_string()),
            "Should find FK column from mod.rs"
        );
    }
    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_empty_module_segments() {
        let temp_dir = TempDir::new().unwrap();
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result = find_fk_column_from_target_entity("crate::Schema", "SomeRelation");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(result.is_none(), "Empty module segments should return None");
    }
    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::nonexistent::Schema", "SomeRelation");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(result.is_none(), "Non-existent file should return None");
    }
    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_unparseable_file() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(models_dir.join("broken.rs"), "this is not valid rust {{{{").unwrap();
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::broken::Schema", "SomeRelation");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(result.is_none(), "Unparseable file should return None");
    }
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::nomodel::Schema", "SomeRelation");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(
            result.is_none(),
            "File without Model struct should return None"
        );
    }
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::comment::Schema", "TargetUser");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(
            result.is_none(),
            "Non-matching relation_enum should return None"
        );
    }
    #[test]
    #[serial]
    fn test_find_fk_column_from_target_entity_tuple_struct() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let model = "pub struct Model(i32, String);";
        std::fs::write(models_dir.join("tuple.rs"), model).unwrap();
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::tuple::Schema", "SomeRelation");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(result.is_none(), "Tuple struct Model should return None");
    }
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::nofrom::Schema", "TargetUser");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(
            result.is_none(),
            "Field without 'from' attribute should return None"
        );
    }
    #[test]
    #[serial]
    fn test_find_struct_candidate_unparseable_file() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();
        std::fs::write(
            src_dir.join("user.rs"),
            "pub struct Model {{{{ broken syntax",
        )
        .unwrap();
        std::fs::write(src_dir.join("valid.rs"), "pub struct Model { pub id: i32 }").unwrap();
        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
        assert!(
            result.is_some(),
            "Should find Model in valid.rs after skipping unparseable candidate user.rs"
        );
    }
    #[test]
    #[serial]
    fn test_find_struct_exact_filename_disambiguation() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();
        std::fs::write(src_dir.join("user.rs"), "pub struct Model { pub id: i32 }").unwrap();
        std::fs::write(
            src_dir.join("user_extended.rs"),
            "pub struct Model { pub name: String }",
        )
        .unwrap();
        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
        assert!(result.is_some(), "Should resolve via exact filename match");
        let (metadata, _) = result.unwrap();
        assert!(
            metadata.definition.contains("id"),
            "Should return user.rs Model (with id field)"
        );
    }
    #[test]
    #[serial]
    fn test_find_struct_no_match_in_candidates_falls_to_rest() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();
        std::fs::write(
            src_dir.join("user.rs"),
            "pub struct Other { pub x: i32 } // Model ref",
        )
        .unwrap();
        std::fs::write(src_dir.join("data.rs"), "pub struct Model { pub id: i32 }").unwrap();
        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
        assert!(
            result.is_some(),
            "Should find Model in data.rs after candidates had no match"
        );
    }
    #[test]
    #[serial]
    fn test_find_struct_full_scan_unparseable_file() {
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path();
        std::fs::write(
            src_dir.join("user.rs"),
            "pub struct Other { pub x: i32 } // Model",
        )
        .unwrap();
        std::fs::write(src_dir.join("broken.rs"), "Model unparseable {{{{{").unwrap();
        std::fs::write(src_dir.join("valid.rs"), "pub struct Model { pub id: i32 }").unwrap();
        let result = find_struct_by_name_in_all_files(src_dir, "Model", Some("UserSchema"));
        assert!(
            result.is_some(),
            "Should find Model in valid.rs after skipping unparseable broken.rs in rest"
        );
    }
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = find_struct_from_path(&ty, None);
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
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
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = find_struct_from_path(&ty, None);
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(result.is_some(), "Should find Model struct via mod.rs path");
        let (metadata, _) = result.unwrap();
        assert!(
            metadata.definition.contains("email"),
            "Should find the correct Model with email field"
        );
    }
    #[test]
    #[serial]
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
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };
        let result =
            find_fk_column_from_target_entity("crate::models::item::Schema", "SomeRelation");
        unsafe {
            if let Some(dir) = original {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
        assert!(
            result.is_none(),
            "Should return None when struct definition fails to parse"
        );
    }
}
