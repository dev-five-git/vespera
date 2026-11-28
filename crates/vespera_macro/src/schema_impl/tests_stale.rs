//! A stored `#[derive(Schema)]` entry whose item left its file (renamed,
//! moved to another file, file deleted) in a long-lived proc-macro server
//! (rust-analyzer) no longer blocks a new item of that name with a false
//! duplicate-name error; a stored item still in its (possibly edited) file
//! keeps the strict error a clean build raises.

use std::path::Path;

use super::*;
use crate::schema_macro::file_cache::bump_epoch;

fn identity(file: &Path, line: usize) -> String {
    format!("{}::User@{line}:11", file.display())
}

fn remove(key: &str) {
    let mut guard = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(bucket) = guard.get_mut(&current_crate_key()) {
        Arc::make_mut(bucket).remove(key);
    }
}

#[rstest::rstest]
#[case::unchanged(Some("pub struct User { id: i32 }\n"), false)]
#[case::edited_still_there(Some("pub struct User { id: i64, extra: u8 }\n"), false)]
#[case::renamed(Some("pub struct Account { id: i32 }\n"), true)]
#[case::moved_out(Some("// User moved to b.rs\n"), true)]
#[case::file_deleted(None, true)]
// R26-SM-L1: a file that no longer parses (mid-edit) still may hold the item.
#[case::unparseable_still_there(Some("pub struct User { id: i64, \nfn broken( {\n"), false)]
fn stale_entry_of_a_changed_file(#[case] first_file_after: Option<&str>, #[case] accepted: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    std::fs::write(&first, "pub struct User { id: i32 }\n").unwrap();
    std::fs::write(&second, "\npub struct User { name: String }\n").unwrap();
    let key = format!("__test_stale_entry_{accepted}_{first_file_after:?}__");
    remove(&key);
    bump_epoch();
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct User { id: i32 }".to_string())
                .with_source_identity(identity(&first, 1)),
        )
        .is_ok()
    );

    if first_file_after != Some("pub struct User { id: i32 }\n") {
        match first_file_after {
            Some(source) => std::fs::write(&first, source).unwrap(),
            None => std::fs::remove_file(&first).unwrap(),
        }
    }
    bump_epoch();
    let result = register_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct User { name: String }".to_string())
            .with_source_identity(identity(&second, 2)),
    );
    assert_eq!(result.is_ok(), accepted);
    assert_eq!(
        current_crate_schemas()[&key].definition.contains("name"),
        accepted
    );
    remove(&key);
}

/// R25-SM0-L1: a variant of a same-named item whose item was deleted from its
/// file (the file changed since the registration) is no longer a variant, so
/// `schema!` raises the missing-derive error a clean build raises instead of
/// reading the stale definition. The other module's variant still resolves.
#[test]
fn variant_of_a_deleted_item_is_no_longer_a_variant() {
    const SOURCE: &str = "pub struct VariantGone { pub limit: u32 }\n";
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod first;\npub mod second;\n").unwrap();
    let first = src.join("first.rs");
    let second = src.join("second.rs");
    std::fs::write(&first, SOURCE).unwrap();
    std::fs::write(&second, SOURCE).unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    bump_epoch();

    let metadata = |file: &Path| {
        StructMetadata::new("VariantGone".to_string(), SOURCE.trim().to_string())
            .with_source_identity(format!("{}::VariantGone@1:11", file.display()))
    };
    let first_module = ["crate", "first"].map(String::from);
    let second_module = ["crate", "second"].map(String::from);
    assert!(register_schema("VariantGone".to_string(), metadata(&first)).is_ok());
    assert!(register_schema("VariantGone".to_string(), metadata(&second)).is_ok());
    assert!(variants::in_module("VariantGone", &second_module).is_some());

    std::fs::write(&second, "// VariantGone was deleted\n").unwrap();
    bump_epoch();
    assert!(variants::in_module("VariantGone", &second_module).is_none());
    assert!(variants::in_module("VariantGone", &first_module).is_some());
}

/// R26-SM-L1: a same-named variant whose file no longer parses (an unbalanced
/// brace typed mid-edit) is still a variant. Dropping it made `schema_type!`
/// read the other module's same-named item instead of the imported one.
#[test]
fn variant_of_an_unparseable_file_is_kept() {
    const SOURCE: &str = "pub struct VariantBroken { pub limit: u32 }\n";
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod first;\npub mod second;\n").unwrap();
    let first = src.join("first.rs");
    let second = src.join("second.rs");
    std::fs::write(&first, SOURCE).unwrap();
    std::fs::write(&second, SOURCE).unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    bump_epoch();

    let metadata = |file: &Path| {
        StructMetadata::new("VariantBroken".to_string(), SOURCE.trim().to_string())
            .with_source_identity(format!("{}::VariantBroken@1:11", file.display()))
    };
    let first_module = ["crate", "first"].map(String::from);
    assert!(register_schema("VariantBroken".to_string(), metadata(&first)).is_ok());
    assert!(register_schema("VariantBroken".to_string(), metadata(&second)).is_ok());

    std::fs::write(
        &first,
        "pub struct VariantBroken { pub limit: u32 \nfn broken( {\n",
    )
    .unwrap();
    bump_epoch();
    assert!(variants::in_module("VariantBroken", &first_module).is_some());
}

/// R34-SM-L1: two same-named items in sibling inline modules of ONE file. Deleting
/// `mod a`'s item leaves `mod b`'s resolving: the staleness of a file's item is
/// decided per module, not by the name over the whole file.
#[test]
fn deleted_item_of_one_module_leaves_its_sibling_resolving() {
    const SOURCE: &str = "pub mod a {\n    pub struct StaleModuleTwin { pub limit: u32 }\n}\n\
        pub mod b {\n    pub struct StaleModuleTwin { pub limit: u32 }\n}\n";
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod pair;\n").unwrap();
    let pair = src.join("pair.rs");
    std::fs::write(&pair, SOURCE).unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    bump_epoch();

    let metadata = |line: usize| {
        StructMetadata::new(
            "StaleModuleTwin".to_string(),
            "pub struct StaleModuleTwin { pub limit: u32 }".to_string(),
        )
        .with_source_identity(format!("{}::StaleModuleTwin@{line}:5", pair.display()))
    };
    let module_a = ["crate", "pair", "a"].map(String::from);
    let module_b = ["crate", "pair", "b"].map(String::from);
    assert!(register_schema("StaleModuleTwin".to_string(), metadata(2)).is_ok());
    assert!(register_schema("StaleModuleTwin".to_string(), metadata(5)).is_ok());
    assert!(variants::in_module("StaleModuleTwin", &module_a).is_some());
    assert!(variants::in_module("StaleModuleTwin", &module_b).is_some());

    // Same line count: `mod a` keeps its braces, its item is gone.
    std::fs::write(
        &pair,
        "pub mod a {\n    // StaleModuleTwin was deleted\n}\n\
        pub mod b {\n    pub struct StaleModuleTwin { pub limit: u32 }\n}\n",
    )
    .unwrap();
    bump_epoch();
    assert!(variants::in_module("StaleModuleTwin", &module_a).is_none());
    assert!(variants::in_module("StaleModuleTwin", &module_b).is_some());
}

/// SM0-L1 (IDE-only): `mod a { struct User }` moved to the file's top level,
/// with a new `mod b { struct User }` beside it, is a duplicate name. The
/// stored entry's own module no longer declares `User`, but the file does, so
/// the entry has not left its file and the strict error is raised.
#[test]
fn item_moved_out_of_its_module_beside_a_same_named_twin_is_a_duplicate() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("moved_twin.rs");
    std::fs::write(
        &file,
        "pub mod a {\n    pub struct MovedTwin { id: i32 }\n}\n",
    )
    .unwrap();
    let identity = format!("{}::MovedTwin@2:15", file.display());
    remove("MovedTwin");
    bump_epoch();
    assert!(
        register_schema(
            "MovedTwin".to_string(),
            StructMetadata::new(
                "MovedTwin".to_string(),
                "struct MovedTwin { id: i32 }".to_string()
            )
            .with_source_identity(identity),
        )
        .is_ok()
    );

    std::fs::write(
        &file,
        "pub struct MovedTwin { id: i32 }\npub mod b {\n    pub struct MovedTwin { name: String }\n}\n",
    )
    .unwrap();
    bump_epoch();
    let result = register_schema(
        "MovedTwin".to_string(),
        StructMetadata::new(
            "MovedTwin".to_string(),
            "struct MovedTwin { name: String }".to_string(),
        )
        .with_source_identity(format!("{}::MovedTwin@1:11", file.display())),
    );
    remove("MovedTwin");
    assert!(result.is_err());
}

/// SM0-L2 (IDE-only): a `schema_type!`-generated DTO has no textual declaration.
/// It has left its file only when the invocation that names it at its recorded
/// position is gone, not after every edit of the file.
#[test]
fn schema_type_entry_is_left_only_when_its_invocation_is_gone() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("dto.rs");
    std::fs::write(&file, "schema_type!(UserDto from Model);\n").unwrap();
    bump_epoch();
    let identity = format!("{}::UserDto@1:14", file.display());
    super::stale::record(Some(&identity));
    let stored = StructMetadata {
        source_identity: Some(identity),
        ..StructMetadata::new("UserDto".to_string(), String::new())
    };

    std::fs::write(
        &file,
        "schema_type!(UserDto from Model, omit = [\"id\"]);\n",
    )
    .unwrap();
    bump_epoch();
    assert!(!super::stale::left_its_file(&stored));

    std::fs::write(
        &file,
        "// UserDto moved\n\nschema_type!(UserDto from Model);\n",
    )
    .unwrap();
    bump_epoch();
    assert!(super::stale::left_its_file(&stored));

    std::fs::write(&file, "// UserDto was removed\n").unwrap();
    bump_epoch();
    assert!(super::stale::left_its_file(&stored));
}

/// SM0-L2 (IDE-only): every re-expansion of a moved `schema_type!` replaces its
/// record, so the fingerprint map holds one entry for the DTO, not one per edit.
#[test]
fn moved_schema_type_invocation_keeps_one_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("grow.rs");
    for line in [1, 2, 3] {
        let source = format!(
            "{}schema_type!(GrowDto from Model);\n",
            "\n".repeat(line - 1)
        );
        std::fs::write(&file, source).unwrap();
        bump_epoch();
        super::stale::record(Some(&format!("{}::GrowDto@{line}:14", file.display())));
    }
    let entries = super::stale::FINGERPRINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .filter(|key| key.starts_with(&format!("{}::GrowDto", file.display())))
        .count();
    assert_eq!(entries, 1);
}

/// A deleted `#[derive(Schema)]` keeps its record (it is not a generated
/// entry), so a `schema_type!` of the same name registered later at another
/// line does not turn the deleted derive into a duplicate.
#[test]
fn deleted_derive_is_left_when_a_generated_twin_takes_its_name() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("guard.rs");
    std::fs::write(&file, "pub struct GuardDto { id: i32 }\n").unwrap();
    remove("GuardDto");
    bump_epoch();
    assert!(
        register_schema(
            "GuardDto".to_string(),
            StructMetadata::new(
                "GuardDto".to_string(),
                "struct GuardDto { id: i32 }".to_string()
            )
            .with_source_identity(format!("{}::GuardDto@1:12", file.display())),
        )
        .is_ok()
    );

    std::fs::write(&file, "\nschema_type!(GuardDto from Model);\n").unwrap();
    bump_epoch();
    let result = register_schema(
        "GuardDto".to_string(),
        StructMetadata::new(
            "GuardDto".to_string(),
            "struct GuardDto { name: String }".to_string(),
        )
        .with_source_identity(format!("{}::GuardDto@2:14", file.display())),
    );
    remove("GuardDto");
    assert!(result.is_ok());
}

/// SM0-L1 (round 40, IDE-only): a derived entry whose item is replaced on its
/// own line by a `schema_type!` naming the same ident has left its position:
/// the invocation is not the item (only a generated record's invocation counts).
/// A derived item edited in place on that line is still declared.
#[test]
fn derive_replaced_by_schema_type_on_its_line_has_left_the_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("swap.rs");
    std::fs::write(&file, "#[derive(Schema)] pub struct User { id: i32 }\n").unwrap();
    bump_epoch();
    let identity = identity(&file, 1);
    super::stale::record(Some(&identity));
    let stored = StructMetadata {
        source_identity: Some(identity),
        ..StructMetadata::new("User".to_string(), String::new())
    };

    std::fs::write(&file, "#[derive(Schema)] pub struct User { id: i64 }\n").unwrap();
    bump_epoch();
    assert!(!super::stale::left_its_file(&stored));

    std::fs::write(&file, "schema_type!(User from Model);\n").unwrap();
    bump_epoch();
    assert!(super::stale::left_its_file(&stored));
}

/// SM0-L1 (round 40), variant scope: the same swap inside the inline module
/// the derive sat in.
#[test]
fn derive_replaced_by_schema_type_in_its_module_has_left_the_module() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("swap_mod.rs");
    std::fs::write(
        &file,
        "mod m { #[derive(Schema)] pub struct User { id: i32 } }\n",
    )
    .unwrap();
    bump_epoch();
    let identity = identity(&file, 1);
    super::stale::record(Some(&identity));
    let stored = StructMetadata {
        source_identity: Some(identity),
        ..StructMetadata::new("User".to_string(), String::new())
    };

    std::fs::write(&file, "mod m { schema_type!(User from Model); }\n").unwrap();
    bump_epoch();
    assert!(super::stale::left_its_module(&stored));

    std::fs::write(
        &file,
        "mod m { #[derive(Schema)] pub struct User { id: i64 } }\n",
    )
    .unwrap();
    bump_epoch();
    assert!(!super::stale::left_its_module(&stored));
}

/// R41-SM0-L1: a variant of `a` is not kept by a `schema_type!` naming the same
/// item in a sibling module `b` on the line `a`'s invocation was recorded at.
#[test]
fn sibling_module_invocation_on_the_same_line_does_not_keep_a_variant() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("sibling_line.rs");
    std::fs::write(
        &file,
        "mod a {\n    schema_type!(User from Model);\n}\nmod b {\n    schema_type!(User from Model);\n}\n",
    )
    .unwrap();
    bump_epoch();
    let identity = identity(&file, 2);
    super::stale::record(Some(&identity));
    let stored = StructMetadata {
        source_identity: Some(identity),
        ..StructMetadata::new("User".to_string(), String::new())
    };

    // `a`'s invocation is gone; `b`'s now sits on the line `a`'s was recorded at.
    std::fs::write(
        &file,
        "mod a {}\nmod b { schema_type!(User from Model); }\n",
    )
    .unwrap();
    bump_epoch();
    assert!(super::stale::left_its_module(&stored));

    // `a`'s own invocation back on that line: still a variant of `a`.
    std::fs::write(
        &file,
        "mod a {\n    schema_type!(User from Model);\n}\nmod b {}\n",
    )
    .unwrap();
    bump_epoch();
    assert!(!super::stale::left_its_module(&stored));
}
