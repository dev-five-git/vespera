//! Item identity (`source_identity`) and item-count tests: a move within a
//! file is a replacement, same-named items of one file conflict, and
//! `count_items_named` ignores items a `#[cfg]` rules out.

use super::tests::remove_current_crate_schema;
use super::*;

#[test]
fn build_struct_metadata_omits_identity_without_call_site_file() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct User { id: i32 }
    };

    let metadata = build_struct_metadata(&input, "UserSchema".to_string(), None);

    assert_eq!(metadata.name, "UserSchema");
    assert!(metadata.definition.contains("struct User"));
    assert_eq!(metadata.source_identity, None);
}

#[test]
fn build_struct_metadata_attaches_identity_from_call_site_file() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct User { id: i32 }
    };
    let call_site_file = Path::new("some/path.rs");

    let metadata = build_struct_metadata(&input, "UserSchema".to_string(), Some(call_site_file));

    assert_eq!(metadata.name, "UserSchema");
    let identity = metadata.source_identity.expect("identity");
    assert!(
        identity.starts_with("some/path.rs::User@"),
        "identity carries the ident position: {identity}"
    );
}

/// Same-named structs in two inline `mod`s of ONE file get distinct
/// identities (span position), so the second registration is rejected as a
/// duplicate instead of silently replacing the first.
#[test]
fn same_named_structs_in_inline_mods_of_one_file_conflict() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("models.rs");
    let source =
        "mod a {\n    struct User { id: i32 }\n}\nmod b {\n    struct User { name: String }\n}\n";
    std::fs::write(&file, source).unwrap();
    let parsed: syn::File = syn::parse_str(source).unwrap();
    let structs: Vec<syn::DeriveInput> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) => module.content.as_ref().map(|(_, items)| &items[0]),
            _ => None,
        })
        .map(|item| syn::parse2(quote::quote!(#item)).unwrap())
        .collect();
    let key = "__test_inline_mod_duplicate__".to_string();
    remove_current_crate_schema(&key);

    let first = build_struct_metadata(&structs[0], key.clone(), Some(&file));
    let second = build_struct_metadata(&structs[1], key.clone(), Some(&file));
    assert_ne!(first.source_identity, second.source_identity);
    assert!(register_schema(key.clone(), first).is_ok());
    assert!(
        register_schema(key.clone(), second).is_err(),
        "two different `User` structs in one file must conflict"
    );
    remove_current_crate_schema(&key);
}

/// A single item whose position changed (IDE edit) is still a replacement;
/// when the file holds zero (not locatable) or several items of that name,
/// the move is ambiguous and the strict `line:column` identity applies.
#[rstest::rstest]
#[case::single_item("\n\nstruct User { id: i64 }\n", true)]
#[case::not_found("\n\nstruct Other { id: i64 }\n", false)]
#[case::second_in_fn_body("fn f() { struct User; }\nstruct User { id: i64 }\n", false)]
fn moved_item_replacement(#[case] source: &str, #[case] replaced: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("models.rs");
    std::fs::write(&file, source).unwrap();
    let path = file.display();
    let key = format!("__test_moved_item_{replaced}_{}__", source.len());
    remove_current_crate_schema(&key);

    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct User { id: i32 }".to_string())
                .with_source_identity(format!("{path}::User@1:7")),
        )
        .is_ok()
    );
    let second = register_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct User { id: i64 }".to_string())
            .with_source_identity(format!("{path}::User@3:7")),
    );
    assert_eq!(second.is_ok(), replaced);
    assert_eq!(
        current_crate_schemas()[&key].definition.contains("i64"),
        replaced
    );
    remove_current_crate_schema(&key);
}

#[rstest::rstest]
#[case::nested_mods("mod a { struct X; mod b { enum X {} } } struct X;", 3)]
#[case::other_names("struct Y; mod a { struct Z; }", 0)]
#[case::external_mod("mod a; struct X;", 1)]
#[case::fn_body("fn f() { struct X; } struct X;", 2)]
#[case::impl_and_const("impl T { fn g() { enum X {} } } const C: () = { struct X; };", 2)]
#[case::static_and_nested_block("static S: u8 = { if true { struct X; } 0 };", 1)]
// L5 (round 18): a twin (or module) `#[cfg]` rules out is not counted;
// an unknown predicate still is.
#[case::inactive_twin(
    "#[cfg(any())] struct X; #[cfg(not(any()))] struct X; #[cfg(unix)] enum X {}",
    2
)]
#[case::inactive_module(
    "#[cfg(any())] mod a { struct X; } #[cfg(all())] mod b { struct X; }",
    1
)]
// R32-SM0-L1: a `cfg_attr(p, cfg(q))` the compiler drops is not counted either.
#[case::inactive_cfg_attr_twin("#[cfg_attr(all(), cfg(any()))] struct X; struct X;", 1)]
#[case::inactive_cfg_attr_module("#[cfg_attr(all(), cfg(any()))] mod a { struct X; } struct X;", 1)]
#[case::active_cfg_attr_twin("#[cfg_attr(any(), cfg(any()))] struct X; struct X;", 2)]
// S20-0-2 (round 20): a `#[cfg]` that is false on a fn / impl / trait /
// const / static hides the items declared inside its body too.
// An active trait is walked too (its default method body holds an item).
#[case::active_trait("trait T { fn g() { struct X; } } struct X;", 2)]
#[case::inactive_fn("#[cfg(any())] fn f() { struct X; } struct X;", 1)]
#[case::inactive_impl("#[cfg(any())] impl T { fn g() { enum X {} } } struct X;", 1)]
#[case::inactive_trait("#[cfg(any())] trait T { fn g() { struct X; } } struct X;", 1)]
#[case::inactive_const("#[cfg(any())] const C: () = { struct X; }; struct X;", 1)]
#[case::inactive_static("#[cfg(any())] static S: u8 = { struct X; 0 }; struct X;", 1)]
// R21-SM0-1 (round 21): the same holds for a method or associated const of an
// impl / trait block; an active method is the control.
#[case::impl_method_control("impl T { #[cfg(all())] fn g() { struct X; } } struct X;", 2)]
#[case::inactive_impl_method("impl T { #[cfg(any())] fn g() { struct X; } } struct X;", 1)]
#[case::inactive_trait_method("trait T { #[cfg(any())] fn g() { struct X; } } struct X;", 1)]
#[case::inactive_impl_const("impl T { #[cfg(any())] const C: () = { struct X; }; } struct X;", 1)]
#[case::active_impl_const_control(
    "impl T { #[cfg(all())] const C: () = { struct X; }; } struct X;",
    2
)]
#[case::inactive_trait_const("trait T { #[cfg(any())] const C: () = { struct X; }; } struct X;", 1)]
#[case::active_trait_const_control(
    "trait T { #[cfg(all())] const C: () = { struct X; }; } struct X;",
    2
)]
fn count_items_named_descends_into_nested_items(#[case] source: &str, #[case] expected: usize) {
    let parsed: syn::File = syn::parse_str(source).unwrap();
    assert_eq!(count_items_named(&parsed.items, "X"), expected);
}

/// R23-SM1: a span without a line (no position reported) for the derive's
/// input, so the identity falls back to the item's own tokens. Two same-named
/// structs of one file with different bodies then keep distinct identities,
/// and the second registration is a duplicate instead of a silent replacement.
#[test]
fn positionless_same_named_items_keep_distinct_identities() {
    let _positions = hide_positions();
    let first: syn::DeriveInput = syn::parse_str("struct User { id: i32 }").unwrap();
    let second: syn::DeriveInput = syn::parse_str("struct User { name: String }").unwrap();
    let file = Path::new("lib.rs");
    let first = derive_source_identity(&first, Some(file)).expect("identity");
    let second = derive_source_identity(&second, Some(file)).expect("identity");
    assert!(first.starts_with("lib.rs::User@tokens:"), "{first}");
    assert_ne!(first, second);
}

/// The same item (identical tokens) gets the same identity: the suffix is a
/// function of the item, not of when it was expanded.
#[test]
fn positionless_identity_is_stable_for_the_same_item() {
    let _positions = hide_positions();
    let one: syn::DeriveInput = syn::parse_str("struct User { id: i32 }").unwrap();
    let two: syn::DeriveInput = syn::parse_str("struct User { id: i32 }").unwrap();
    let file = Path::new("lib.rs");
    assert_eq!(
        derive_source_identity(&one, Some(file)),
        derive_source_identity(&two, Some(file))
    );
}
