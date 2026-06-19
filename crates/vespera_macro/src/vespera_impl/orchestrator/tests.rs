    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    // ========== Tests for process_vespera_macro ==========

    #[test]
    fn test_process_vespera_macro_folder_not_found() {
        let processed = ProcessedVesperaInput {
            folder_name: "nonexistent_folder_xyz_123".to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };
        let result = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("route folder") && err.contains("not found"));
    }

    #[test]
    fn test_process_vespera_macro_collect_metadata_error() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an invalid route file (will cause parse error but collect_metadata handles it)
        create_temp_file(&temp_dir, "invalid.rs", "not valid rust code {{{");

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: Some("Test API".to_string()),
            version: Some("1.0.0".to_string()),
            docs_url: None,
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        // This exercises the collect_metadata path (which handles parse errors gracefully)
        let result = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());
        // Result may succeed or fail depending on how collect_metadata handles invalid files
        let _ = result;
    }

    #[test]
    fn test_process_vespera_macro_with_schema_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty file (valid but no routes)
        create_temp_file(&temp_dir, "empty.rs", "// empty file\n");

        let schema_storage = HashMap::from([(
            "TestSchema".to_string(),
            StructMetadata::new(
                "TestSchema".to_string(),
                "struct TestSchema { id: i32 }".to_string(),
            ),
        )]);

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: Some("/docs".to_string()),
            redoc_url: Some("/redoc".to_string()),
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        // This exercises the schema_storage extend path
        let result = process_vespera_macro(&processed, &schema_storage, &[], Span::call_site());
        // We only care about exercising the code path
        let _ = result;
    }

    #[test]
    #[serial_test::serial]
    fn test_process_vespera_macro_with_cron_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create src/ subfolder structure to simulate a real project
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(src_dir.join("routes")).expect("create routes dir");
        std::fs::write(src_dir.join("routes").join("health.rs"), "// empty\n")
            .expect("write health.rs");

        // Set CARGO_MANIFEST_DIR so module path derivation works
        let old_manifest = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe {
            std::env::set_var(
                "CARGO_MANIFEST_DIR",
                temp_dir.path().to_string_lossy().as_ref(),
            );
        }

        // Populate CRON_STORAGE with a fake cron entry
        {
            let mut storage = crate::CRON_STORAGE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            storage.push(crate::cron_impl::StoredCronInfo {
                fn_name: "test_cron_job".to_string(),
                expression: "0 */5 * * * *".to_string(),
                file_path: Some(
                    src_dir
                        .join("routes")
                        .join("health.rs")
                        .display()
                        .to_string(),
                ),
            });
        }

        let processed = ProcessedVesperaInput {
            folder_name: src_dir.join("routes").to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        // This exercises the CRON_STORAGE → CronMetadata derivation path
        let result = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());
        assert!(
            result.is_ok(),
            "Should succeed with cron storage: {result:?}"
        );

        // Clean up CRON_STORAGE
        {
            let mut storage = crate::CRON_STORAGE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            storage.retain(|s| s.fn_name != "test_cron_job");
        }

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(val) = old_manifest {
                std::env::set_var("CARGO_MANIFEST_DIR", val);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }
    }

    // ========== Tests for process_export_app ==========

    #[test]
    fn test_process_export_app_folder_not_found() {
        let name: syn::Ident = syn::parse_quote!(TestApp);
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let result = process_export_app(
            &name,
            "nonexistent_folder_xyz",
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("route folder") && err.contains("not found"));
    }

    #[test]
    fn test_process_export_app_with_empty_folder() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty file
        create_temp_file(&temp_dir, "empty.rs", "// empty\n");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        // This exercises collect_metadata and other paths
        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );
        // We only care about exercising the code path
        let _ = result;
    }

    #[test]
    fn test_process_export_app_with_schema_storage() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty but valid Rust file
        create_temp_file(&temp_dir, "mod.rs", "// module file\n");

        let schema_storage = HashMap::from([(
            "AppSchema".to_string(),
            StructMetadata::new(
                "AppSchema".to_string(),
                "struct AppSchema { name: String }".to_string(),
            ),
        )]);

        let name: syn::Ident = syn::parse_quote!(MyExportedApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &schema_storage,
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );
        // Exercises the schema_storage.extend path
        let _ = result;
    }

    #[test]
    fn test_process_export_app_collect_metadata_error() {
        // Lines 210-212: collect_metadata returns error for invalid Rust syntax
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create a file with invalid Rust syntax that will cause parse error
        create_temp_file(&temp_dir, "invalid.rs", "fn broken( { syntax error");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to scan route folder"));
    }

    #[test]
    fn test_process_export_app_create_dir_error() {
        // Lines 232-234: create_dir_all failure when path contains a file
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty valid Rust file
        create_temp_file(&temp_dir, "empty.rs", "// empty file\n");

        // Create target directory but make 'vespera' a file instead of directory
        let target_dir = temp_dir.path().join("target");
        fs::create_dir(&target_dir).expect("Failed to create target dir");
        fs::write(target_dir.join("vespera"), "blocking file").expect("Failed to write file");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to create build cache directory"));
    }

    #[test]
    fn test_process_export_app_write_spec_error() {
        // Lines 239-241: fs::write failure when spec file path is a directory
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create an empty valid Rust file
        create_temp_file(&temp_dir, "empty.rs", "// empty file\n");

        // Create target/vespera directory and make spec file name a directory
        let vespera_dir = temp_dir.path().join("target").join("vespera");
        fs::create_dir_all(&vespera_dir).expect("Failed to create vespera dir");
        // Create a directory where the spec file should be written
        fs::create_dir(vespera_dir.join("TestApp.openapi.json"))
            .expect("Failed to create blocking dir");

        let name: syn::Ident = syn::parse_quote!(TestApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to write OpenAPI spec file"));
    }
    #[test]
    fn test_process_vespera_macro_no_openapi_output() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(&temp_dir, "empty.rs", "// empty route file\n");

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        let result = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());
        assert!(
            result.is_ok(),
            "Should succeed with no openapi output configured"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_process_vespera_macro_with_profiling() {
        let old_profile = std::env::var("VESPERA_PROFILE").ok();
        unsafe { std::env::set_var("VESPERA_PROFILE", "1") };

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(&temp_dir, "empty.rs", "// empty\n");

        let processed = ProcessedVesperaInput {
            folder_name: temp_dir.path().to_string_lossy().to_string(),
            openapi_file_names: vec![],
            title: None,
            version: None,
            docs_url: None,
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        let result = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());

        // Restore
        unsafe {
            if let Some(val) = old_profile {
                std::env::set_var("VESPERA_PROFILE", val);
            } else {
                std::env::remove_var("VESPERA_PROFILE");
            }
        };

        assert!(result.is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn test_process_export_app_with_profiling() {
        let old_profile = std::env::var("VESPERA_PROFILE").ok();
        unsafe { std::env::set_var("VESPERA_PROFILE", "1") };

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(&temp_dir, "empty.rs", "// empty\n");

        let name: syn::Ident = syn::parse_quote!(TestProfileApp);
        let folder_path = temp_dir.path().to_string_lossy().to_string();

        let result = process_export_app(
            &name,
            &folder_path,
            &HashMap::new(),
            &temp_dir.path().to_string_lossy(),
            &[],
            Span::call_site(),
        );

        // Restore
        unsafe {
            if let Some(val) = old_profile {
                std::env::set_var("VESPERA_PROFILE", val);
            } else {
                std::env::remove_var("VESPERA_PROFILE");
            }
        };

        // Exercise the code path
        let _ = result;
    }

    #[test]
    #[serial_test::serial]
    fn test_process_vespera_macro_cache_hit() {
        // Exercises lines 320-324, 327, 329: the cache_hit branch in process_vespera_macro.
        // First call populates the cache, second call hits it.
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        create_temp_file(
            &temp_dir,
            "users.rs",
            "pub async fn list_users() -> String { \"users\".to_string() }\n",
        );

        let folder_path = temp_dir.path().to_string_lossy().to_string();
        let openapi_path = temp_dir.path().join("openapi.json");

        // Set CARGO_MANIFEST_DIR so cache path resolves to temp_dir/target/vespera/
        let old_manifest = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let processed = ProcessedVesperaInput {
            folder_name: folder_path.clone(),
            openapi_file_names: vec![openapi_path.to_string_lossy().to_string()],
            title: Some("Test API".to_string()),
            version: Some("1.0.0".to_string()),
            docs_url: Some("/docs".to_string()),
            redoc_url: None,
            servers: None,
            security_schemes: None,
            security: None,
            tag_descriptions: None,
            merge: vec![],
        };

        // First call: cache MISS — scans files, generates spec, writes cache
        let result1 = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());
        assert!(
            result1.is_ok(),
            "First call (cache miss) should succeed: {:?}",
            result1.err()
        );
        assert!(
            openapi_path.exists(),
            "openapi.json should be written on first call"
        );

        // Second call: cache HIT — exercises lines 320-324, 327, 329
        let result2 = process_vespera_macro(&processed, &HashMap::new(), &[], Span::call_site());
        assert!(
            result2.is_ok(),
            "Second call (cache hit) should succeed: {:?}",
            result2.err()
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(val) = old_manifest {
                std::env::set_var("CARGO_MANIFEST_DIR", val);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        };
    }
