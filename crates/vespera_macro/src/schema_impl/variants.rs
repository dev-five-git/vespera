//! Same-named `#[derive(Schema)]` items of different modules whose stored
//! definitions are identical (`mod first { struct Page { .. } }` and `mod
//! second { struct Page { .. } }`).
//!
//! [`super::SCHEMA_STORAGE`] is keyed by the schema name and keeps the last
//! of them (they are one OpenAPI component). Their statically extracted
//! `field_defaults` can still differ (each `default = "limit"` names its own
//! module's function), so every such item is also kept here, one per module,
//! and `schema!(first::Page)` reads the item of the module it names instead
//! of whichever `Page` was derived last.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::metadata::StructMetadata;
use crate::schema_macro::storage_module_path;

/// `crate key -> schema name -> (defining module, metadata)`.
type Variants = HashMap<String, HashMap<String, Vec<(Vec<String>, StructMetadata)>>>;

static VARIANTS: LazyLock<Mutex<Variants>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keep `metadata` (registered under `name`) as the variant of its module,
/// replacing an earlier one of that module; an item whose module is unknown
/// is not kept.
pub(super) fn record(name: &str, metadata: &StructMetadata) {
    let Some(module) = storage_module_path(metadata) else {
        return;
    };
    let mut variants = VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entries = variants
        .entry(super::current_crate_key())
        .or_default()
        .entry(name.to_string())
        .or_default();
    entries.retain(|(defined_in, _)| *defined_in != module);
    entries.push((module, metadata.clone()));
}

/// An already registered item of `name` re-expanded as itself (`previous`
/// -> `metadata`: an IDE edit, or the item moved): when `name` has
/// variants, the item's variant follows the edit (it kept the old metadata),
/// and `Err` when another module's variant no longer has its definition
/// (the duplicate-name error a clean build raises). Nothing changes on `Err`.
pub(super) fn update(
    name: &str,
    previous: &StructMetadata,
    metadata: &StructMetadata,
) -> Result<(), ()> {
    let Some(module) = storage_module_path(metadata) else {
        return Ok(());
    };
    let previous_module = storage_module_path(previous);
    let mut variants = VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entries) = variants
        .get_mut(&super::current_crate_key())
        .and_then(|names| names.get_mut(name))
    else {
        return Ok(());
    };
    let is_this_item = |defined_in: &Vec<String>| {
        *defined_in == module || previous_module.as_ref() == Some(defined_in)
    };
    if entries.iter().any(|(defined_in, other)| {
        !is_this_item(defined_in) && other.definition != metadata.definition
    }) {
        return Err(());
    }
    entries.retain(|(defined_in, _)| !is_this_item(defined_in));
    entries.push((module, metadata.clone()));
    Ok(())
}

/// The variant of `name` defined in `module`, when same-named items of
/// several modules were registered.
pub fn in_module(name: &str, module: &[String]) -> Option<StructMetadata> {
    VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&super::current_crate_key())?
        .get(name)?
        .iter()
        .find(|(defined_in, _)| defined_in == module)
        .map(|(_, metadata)| metadata.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::schema_macro::file_cache::bump_epoch;
    use crate::schema_macro::set_test_call_site_module;

    const LIB: &str = "mod first { pub struct Page { pub limit: u32 } }\n\
        mod second { pub struct Page { pub limit: u32 } }\n\
        use other_crate::types;\n\
        mod api { use crate::first::Page; }\n\
        mod reexp { pub use crate::first::Page; }\n\
        mod api2 { use crate::reexp::Page; }\n\
        mod api3 { use other_crate::Page; }\n";

    fn page(lib: &std::path::Path, line: usize, default: u32) -> StructMetadata {
        StructMetadata {
            source_identity: Some(format!("{}::Page@{line}:24", lib.display())),
            field_defaults: BTreeMap::from([("limit".to_string(), serde_json::json!(default))]),
            ..StructMetadata::new(
                "Page".to_string(),
                "pub struct Page { pub limit: u32 }".to_string(),
            )
        }
    }

    fn schema_of(path: &str) -> String {
        let input: crate::schema_macro::SchemaInput = syn::parse_str(path).unwrap();
        let storage = super::super::current_crate_schemas();
        crate::schema_macro::generate_schema_code(&input, &storage)
            .unwrap()
            .to_string()
    }

    /// R14-X1: two same-named items of different modules with one stored
    /// definition both register (the storage keeps the last), and `schema!`
    /// reads the one of the module its path names — or, for a bare name,
    /// the call site's module; an unknown module falls back to the storage.
    #[test]
    fn same_named_items_are_read_by_module() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib = dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
        std::fs::write(&lib, LIB).unwrap();
        let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();

        assert!(super::super::register_schema("Page".to_string(), page(&lib, 1, 10)).is_ok());
        assert!(super::super::register_schema("Page".to_string(), page(&lib, 2, 20)).is_ok());
        let first = ["crate", "first"].map(String::from);
        let second = ["crate", "second"].map(String::from);
        assert_eq!(
            in_module("Page", &first).unwrap().field_defaults["limit"],
            10
        );
        assert_eq!(
            in_module("Page", &second).unwrap().field_defaults["limit"],
            20
        );
        assert!(in_module("Page", &["crate".to_string()]).is_none());
        assert!(in_module("Other", &first).is_none());

        let call_site = set_test_call_site_module("crate");
        let first_schema = schema_of("first::Page");
        assert!(first_schema.contains("10"), "{first_schema}");
        assert!(!first_schema.contains("20"), "{first_schema}");
        assert!(schema_of("second::Page").contains("20"));
        // Through another crate's import: no module, the storage's last.
        assert!(schema_of("types::Page").contains("20"));
        // A bare name the call site does not know: the storage's last.
        assert!(schema_of("Page").contains("20"));
        drop(call_site);
        let call_site = set_test_call_site_module("crate::first");
        assert!(schema_of("Page").contains("10"));
        drop(call_site);
        // L2 (round 15): a bare name follows the call site's `use` (and a
        // `pub use` re-export) to the defining module; it used to read the
        // last-derived `second::Page`.
        for module in ["crate::api", "crate::api2"] {
            let _call_site = set_test_call_site_module(module);
            let schema = schema_of("Page");
            assert!(
                schema.contains("10") && !schema.contains("20"),
                "{module}: {schema}"
            );
        }
        // Imported from another crate: no variant, the storage's last.
        let _call_site = set_test_call_site_module("crate::api3");
        assert!(schema_of("Page").contains("20"));

        // Re-registering a module replaces its variant; an item without a
        // known module is not kept.
        record("Page", &page(&lib, 1, 11));
        assert_eq!(
            in_module("Page", &first).unwrap().field_defaults["limit"],
            11
        );
        let mut unknown = page(&lib, 1, 12);
        unknown.source_identity = None;
        record("Page", &unknown);
        assert_eq!(
            in_module("Page", &first).unwrap().field_defaults["limit"],
            11
        );

        // L5 (round 15): the stored item re-expanded as itself (an IDE edit)
        // updates its variant (it kept the old metadata) ...
        assert!(super::super::register_schema("Page".to_string(), page(&lib, 2, 21)).is_ok());
        assert_eq!(
            in_module("Page", &second).unwrap().field_defaults["limit"],
            21
        );
        // ... and a definition that no longer matches another module's
        // variant is the duplicate-name error, changing nothing.
        let mut edited = page(&lib, 2, 22);
        edited.definition = "pub struct Page { pub limit: u64 }".to_string();
        assert!(super::super::register_schema("Page".to_string(), edited).is_err());
        assert_eq!(
            in_module("Page", &second).unwrap().field_defaults["limit"],
            21
        );
        // Without a module or without variants: nothing to update.
        assert_eq!(update("Page", &unknown, &unknown), Ok(()));
        assert_eq!(
            update("Other", &page(&lib, 1, 1), &page(&lib, 1, 1)),
            Ok(())
        );
    }
}
