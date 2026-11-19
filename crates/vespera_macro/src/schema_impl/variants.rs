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
/// (the duplicate-name error a clean build raises). On `Err` the variant
/// still follows the edit (pending, the storage is unchanged): when the
/// other module's item is then edited the same way, [`converge`] accepts
/// it (L1, round 16: both items kept rejecting each other's stale variant
/// until the proc-macro server restarted).
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
    let conflict = entries.iter().any(|(defined_in, other)| {
        !is_this_item(defined_in) && other.definition != metadata.definition
    });
    entries.retain(|(defined_in, _)| !is_this_item(defined_in));
    entries.push((module, metadata.clone()));
    if conflict { Err(()) } else { Ok(()) }
}

/// A registration of `name` from another item than the stored one, with
/// another definition: when that item already is a variant of `name` (it
/// was registered as one, then edited), its variant follows the edit, and
/// `true` when every variant now has its definition — the same-named items
/// were all edited alike, so the registration is accepted. `false` (the
/// duplicate-name error) otherwise; an item that is no variant changes
/// nothing.
pub(super) fn converge(name: &str, metadata: &StructMetadata) -> bool {
    let Some(module) = storage_module_path(metadata) else {
        return false;
    };
    let mut variants = VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entries) = variants
        .get_mut(&super::current_crate_key())
        .and_then(|names| names.get_mut(name))
        .filter(|entries| entries.iter().any(|(defined_in, _)| *defined_in == module))
    else {
        return false;
    };
    entries.retain(|(defined_in, _)| *defined_in != module);
    entries.push((module, metadata.clone()));
    entries
        .iter()
        .all(|(_, other)| other.definition == metadata.definition)
}

/// The variant of `name` defined in `module`, when same-named items of
/// several modules were registered. A variant whose item has left its file
/// (deleted or moved since it was registered, R25-SM0-L1) is no longer one:
/// a clean build reports the missing derive for it, so the IDE must too.
pub fn in_module(name: &str, module: &[String]) -> Option<StructMetadata> {
    let found = VARIANTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&super::current_crate_key())?
        .get(name)?
        .iter()
        .find(|(defined_in, _)| defined_in == module)
        .map(|(_, metadata)| metadata.clone())?;
    (!super::stale::left_its_file(&found)).then_some(found)
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
        mod api3 { use other_crate::Page; }\n\
        mod plain { pub struct Page { pub limit: u32 } }\n";

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

    fn schema_error(path: &str) -> String {
        let input: crate::schema_macro::SchemaInput = syn::parse_str(path).unwrap();
        let storage = super::super::current_crate_schemas();
        crate::schema_macro::generate_schema_code(&input, &storage)
            .unwrap_err()
            .to_string()
    }

    /// L3 (round 18): a path through another crate's import, a module whose
    /// `Page` has no `#[derive(Schema)]` and a bare name imported from
    /// another crate are errors (each read the storage's last,
    /// `second::Page`).
    fn unknown_modules_are_errors() {
        let call_site = set_test_call_site_module("crate");
        assert!(schema_error("types::Page").contains("is in another crate"));
        let error = schema_error("plain::Page");
        assert!(
            error.contains(
                "`Page` of `crate::plain` has no `#[derive(Schema)]` in this crate (the `#[derive(Schema)]` `Page` is in `crate::second`)"
            ),
            "{error}"
        );
        drop(call_site);
        let _call_site = set_test_call_site_module("crate::api3");
        let error = schema_error("Page");
        assert!(
            error.contains("`Page` of `other_crate` has no `#[derive(Schema)]`"),
            "{error}"
        );
    }

    /// R14-X1: two same-named items of different modules with one stored
    /// definition both register (the storage keeps the last), and `schema!`
    /// reads the one of the module its path names — or, for a bare name,
    /// the call site's module; a bare name the call site does not resolve
    /// falls back to the storage, a module of another crate or one without
    /// that item derived is an error.
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
        // L2 (round 16): a qualified path through a `pub use` re-export
        // reads the re-exported module's variant (it read the storage's
        // last, `second::Page`).
        let reexported = schema_of("reexp::Page");
        assert!(
            reexported.contains("10") && !reexported.contains("20"),
            "{reexported}"
        );
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
        unknown_modules_are_errors();

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
        // variant is the duplicate-name error (the storage keeps the old
        // item, the variant is pending) ...
        let edit = |line, default| {
            let mut edited = page(&lib, line, default);
            edited.definition = "pub struct Page { pub limit: u64 }".to_string();
            edited
        };
        assert!(super::super::register_schema("Page".to_string(), edit(2, 22)).is_err());
        let stored = |name: &str| {
            super::super::current_crate_schemas()[name]
                .definition
                .clone()
        };
        assert_eq!(stored("Page"), "pub struct Page { pub limit: u32 }");
        assert_eq!(
            in_module("Page", &second).unwrap().field_defaults["limit"],
            22
        );
        // ... L1 (round 16): until the other module's item is edited alike,
        // which then converges (both kept rejecting each other's stale
        // variant until the proc-macro server restarted).
        assert!(super::super::register_schema("Page".to_string(), edit(1, 12)).is_ok());
        assert_eq!(stored("Page"), "pub struct Page { pub limit: u64 }");
        assert!(super::super::register_schema("Page".to_string(), edit(2, 23)).is_ok());
        // The other order: the non-stored item first (pending), then the
        // stored one re-expanded as itself.
        let mut third = page(&lib, 1, 13);
        third.definition = "pub struct Page { pub limit: i8 }".to_string();
        assert!(super::super::register_schema("Page".to_string(), third.clone()).is_err());
        let mut fourth = page(&lib, 2, 24);
        fourth.definition = third.definition.clone();
        assert!(super::super::register_schema("Page".to_string(), fourth).is_ok());
        assert_eq!(stored("Page"), "pub struct Page { pub limit: i8 }");
        // An item that is no variant converges nothing.
        let mut stranger = page(&lib, 5, 1);
        stranger.definition = "pub struct Page { pub other: u8 }".to_string();
        assert!(!converge("Page", &stranger));
        assert!(!converge("Page", &unknown));
        assert!(!converge("Other", &page(&lib, 1, 1)));
        // Without a module or without variants: nothing to update.
        assert_eq!(update("Page", &unknown, &unknown), Ok(()));
        assert_eq!(
            update("Other", &page(&lib, 1, 1), &page(&lib, 1, 1)),
            Ok(())
        );
    }

    /// L3 (round 16): `schema_type!` from a bare name the call site imports
    /// from one of several same-named variants derives from that module's
    /// item (the last-derived `second::Page` was used: its `limit` default
    /// fn, its field types).
    #[test]
    fn schema_type_bare_name_reads_the_imported_variant() {
        const DEFINITION: &str =
            "pub struct Page { # [serde (default = \"limit\")] pub limit : u32 }";
        let dir = tempfile::TempDir::new().unwrap();
        let lib = dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
        std::fs::write(
            &lib,
            "mod first { pub struct Page { pub limit: u32 } pub fn limit() -> u32 { 10 } }\n\
             mod second { pub struct Page { pub limit: u32 } pub fn limit() -> u32 { 20 } }\n\
             mod api { use crate::first::Page; }\n",
        )
        .unwrap();
        let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        for line in [1, 2] {
            let mut metadata = page(&lib, line, 0);
            metadata.definition = DEFINITION.to_string();
            assert!(super::super::register_schema("Page".to_string(), metadata).is_ok());
        }
        let generate = |module: &str| {
            let _call_site = set_test_call_site_module(module);
            let input: crate::schema_macro::SchemaTypeInput =
                syn::parse_str("PageDto from Page").unwrap();
            let storage = super::super::current_crate_schemas();
            crate::schema_macro::generate_schema_type_code(&input, &storage)
                .map(|(tokens, _)| tokens.to_string())
        };
        let output = generate("crate::api").unwrap();
        assert!(output.contains("\"crate::first::limit\""), "{output}");
        assert!(!output.contains("crate::second"), "{output}");
        // A call site the name does not import: the bare name is refused with
        // the path to write (R24-SM2-F1), not read from the storage's last variant.
        let error = generate("crate").unwrap_err().to_string();
        assert!(error.contains("crate::second::Page"), "{error}");
    }
}
