use rstest::rstest;

use super::*;

fn ident(name: &str) -> syn::Ident {
    syn::Ident::new(name, proc_macro2::Span::call_site())
}

fn path(segments: &[&str]) -> Vec<String> {
    segments.iter().map(ToString::to_string).collect()
}

#[rstest]
#[case(&["crate", "models", "memo"], r"pub struct UserSchema { pub id: i32, pub memos: HasMany<memo::Entity>, }", vec![])]
#[case(&["crate", "models", "user"], r"pub struct MemoSchema { pub id: i32, pub user: BelongsTo<user::Entity>, }", vec!["user".to_string()])]
#[case(&["crate", "models", "user"], r"pub struct MemoSchema { pub id: i32, pub user: HasOne<user::Entity>, }", vec!["user".to_string()])]
#[case(&["crate", "models", "user"], r"pub struct MemoSchema { pub id: i32, pub user: Box<user::Schema>, }", vec!["user".to_string()])]
#[case(&["crate", "models", "memo"], r"pub struct UserSchema { pub id: i32, pub name: String, }", vec![])]
fn test_detect_circular_fields(
    #[case] source_module_path: &[&str],
    #[case] related_schema_def: &str,
    #[case] expected: Vec<String>,
) {
    assert_eq!(
        analyze_circular_refs(&path(source_module_path), &[], related_schema_def).circular_fields,
        expected
    );
}

#[test]
fn test_detect_circular_fields_invalid_struct() {
    assert_eq!(
        analyze_circular_refs(&["crate".to_string()], &[], "not valid rust").circular_fields,
        Vec::<String>::new()
    );
}

#[test]
fn test_detect_circular_fields_unnamed_fields() {
    assert_eq!(
        analyze_circular_refs(
            &path(&["crate", "models", "test"]),
            &[],
            "pub struct TupleStruct(i32, String);"
        )
        .circular_fields,
        Vec::<String>::new()
    );
}

#[rstest]
#[case(
    r"pub struct Model { pub id: i32, pub user: BelongsTo<user::Entity>, }",
    true
)]
#[case(
    r"pub struct Model { pub id: i32, pub user: HasOne<user::Entity>, }",
    true
)]
#[case(r"pub struct Model { pub id: i32, pub name: String, }", false)]
#[case(
    r"pub struct Model { pub id: i32, pub items: HasMany<item::Entity>, }",
    false
)]
fn test_has_fk_relations(#[case] model_def: &str, #[case] expected: bool) {
    assert_eq!(
        analyze_circular_refs(&[], &[], model_def).has_fk_relations,
        expected
    );
}

#[test]
fn test_has_fk_relations_invalid_struct() {
    assert!(!analyze_circular_refs(&[], &[], "not valid rust").has_fk_relations);
}

#[test]
fn test_has_fk_relations_unnamed_fields() {
    assert!(
        !analyze_circular_refs(&[], &[], "pub struct TupleStruct(i32, String);").has_fk_relations
    );
}

// Module-aware detection (the related model's module is known): a
// self-referential relation of the source (`parent: BelongsTo<Entity>`)
// and SeaORM 2.0's `BelongsTo<Option<..>>` back-reference are circular;
// a relation to a third module is not (no model on disk to reach back).
#[rstest]
#[case::self_reference(
    &["crate", "models", "category"],
    &["crate", "models", "category"],
    r"pub struct Model { pub id: i32, pub parent: BelongsTo<Option<Entity>>, }",
    vec!["parent".to_string()]
)]
#[case::optional_back_reference(
    &["crate", "models", "cake"],
    &["crate", "models", "bakery"],
    r"pub struct Model { pub id: i32, pub cake: BelongsTo<Option<super::cake::Entity>>, }",
    vec!["cake".to_string()]
)]
#[case::self_reference_of_another_model(
    &["crate", "models", "cake"],
    &["crate", "models", "bakery"],
    r"pub struct Model { pub id: i32, pub parent: HasOne<self::Entity>, }",
    vec![]
)]
#[case::unrelated_module(
    &["crate", "models", "cake"],
    &["crate", "models", "bakery"],
    r"pub struct Model { pub id: i32, pub owner: BelongsTo<super::owner::Entity>, }",
    vec![]
)]
#[case::has_many_back_reference(
    &["crate", "models", "cake"],
    &["crate", "models", "bakery"],
    r"pub struct Model { pub id: i32, pub cakes: HasMany<super::cake::Entity>, }",
    vec![]
)]
fn module_aware_circular_fields(
    #[case] source: &[&str],
    #[case] related: &[&str],
    #[case] definition: &str,
    #[case] expected: Vec<String>,
) {
    assert_eq!(
        analyze_circular_refs(&path(source), &path(related), definition).circular_fields,
        expected
    );
}

/// `a -> b -> c -> a`: from `a`, the related `b`'s `next` reaches `a`
/// through `c` (read from disk), so it is circular; a chain that ends
/// (`d -> e`) is not, and a cycle not through the source (`f <-> g`) ends.
#[test]
fn transitive_cycles_are_circular() {
    let temp = tempfile::TempDir::new().unwrap();
    let models = temp.path().join("src").join("models");
    std::fs::create_dir_all(&models).unwrap();
    for (module, next) in [("c", "a"), ("e", "missing"), ("f", "g"), ("g", "f")] {
        std::fs::write(
            models.join(format!("{module}.rs")),
            format!(
                "pub struct Model {{ pub id: i32, pub next: BelongsTo<Option<super::{next}::Entity>>, pub rows: HasMany<super::a::Entity> }}"
            ),
        )
        .unwrap();
    }
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
    let analysis = |related: &str, definition: &str| {
        analyze_circular_refs(
            &path(&["crate", "models", "a"]),
            &path(&["crate", "models", related]),
            definition,
        )
        .circular_fields
    };
    assert_eq!(
        analysis(
            "b",
            "pub struct Model { pub id: i32, pub next: BelongsTo<super::c::Entity> }"
        ),
        ["next"]
    );
    assert_eq!(
        analysis(
            "d",
            "pub struct Model { pub id: i32, pub next: BelongsTo<super::e::Entity> }"
        )
        .len(),
        0
    );
    assert_eq!(
        analysis(
            "d",
            "pub struct Model { pub id: i32, pub next: BelongsTo<super::f::Entity> }"
        )
        .len(),
        0
    );
    // A model that does not parse ends the walk: no circular field, while
    // the same file made valid (`h -> a`, a real back-reference) is circular.
    std::fs::write(models.join("h.rs"), "pub struct Model { broken").unwrap();
    assert_eq!(
        analysis(
            "d",
            "pub struct Model { pub id: i32, pub next: BelongsTo<super::h::Entity> }"
        )
        .len(),
        0
    );
    std::fs::write(
        models.join("h.rs"),
        "pub struct Model { pub id: i32, pub next: BelongsTo<super::a::Entity> }",
    )
    .unwrap();
    // The file metadata is cached per epoch: the rewrite is seen only after a bump.
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(
        analysis(
            "d",
            "pub struct Model { pub id: i32, pub next: BelongsTo<super::h::Entity> }"
        ),
        ["next"]
    );
}

#[test]
fn test_generate_inline_type_construction_invalid_struct() {
    assert!(
        generate_inline_type_construction(
            &ident("TestInline"),
            &["id".to_string()],
            "not valid rust",
            "model"
        )
        .to_string()
        .contains("Default :: default ()")
    );
}

#[test]
fn test_generate_inline_type_construction_tuple_struct() {
    assert!(
        generate_inline_type_construction(
            &ident("TestInline"),
            &["id".to_string()],
            "pub struct TupleStruct(i32, String);",
            "model"
        )
        .to_string()
        .contains("Default :: default ()")
    );
}

// The exact construction is the contract: included fields only, in
// declaration order, relation / `serde(skip)` fields never, and a field's
// `#[cfg(..)]` kept on its assignment.
#[rstest]
#[case::with_fields(
    &["id", "name"],
    r"pub struct Model { pub id: i32, pub name: String, pub email: String, }",
    "UserInline { id : r . id , name : r . name }"
)]
#[case::skips_relations(
    &["id", "memos"],
    r"pub struct Model { pub id: i32, pub memos: HasMany<memo::Entity>, }",
    "UserInline { id : r . id }"
)]
#[case::skips_serde_skip(
    &["id", "internal"],
    r"pub struct Model { pub id: i32, #[serde(skip)] pub internal: String, }",
    "UserInline { id : r . id }"
)]
#[case::empty_included(
    &[],
    r"pub struct Model { pub id: i32, pub name: String, }",
    "UserInline { }"
)]
#[case::keeps_cfg(
    &["id", "extra"],
    r#"pub struct Model { pub id: i32, #[cfg(feature = "x")] #[doc = "d"] pub extra: i32, }"#,
    "UserInline { id : r . id , # [cfg (feature = \"x\")] extra : r . extra }"
)]
// M2 (round 16): the inline type hoists a `cfg_attr(p, cfg(q))`, so the
// assignment is gated alike (it was emitted unconditionally: E0560).
#[case::hoists_cfg_attr_cfg(
    &["id", "note"],
    r#"pub struct Model { pub id: i32, #[cfg_attr(feature = "a", cfg(feature = "b"), doc = "d")] pub note: String, }"#,
    "UserInline { id : r . id , # [cfg (any (not (feature = \"a\") , feature = \"b\"))] note : r . note }"
)]
fn inline_type_construction(
    #[case] included: &[&str],
    #[case] definition: &str,
    #[case] expected: &str,
) {
    let included: Vec<String> = included.iter().map(ToString::to_string).collect();
    assert_eq!(
        generate_inline_type_construction(&ident("UserInline"), &included, definition, "r")
            .to_string(),
        expected
    );
}

#[test]
fn test_circular_fields_empty_module_path() {
    assert_eq!(
        analyze_circular_refs(&[], &[], "pub struct Schema { pub id: i32 }").circular_fields,
        Vec::<String>::new()
    );
}

#[rstest]
#[case::option_box(r"pub struct UserSchema { pub id: i32, pub memo: Option<Box<memo::Schema>>, }")]
#[case::schema_suffix(r"pub struct UserSchema { pub id: i32, pub memo: Box<MemoSchema>, }")]
fn schema_reference_patterns_are_circular(#[case] definition: &str) {
    assert_eq!(
        analyze_circular_refs(&path(&["crate", "models", "memo"]), &[], definition).circular_fields,
        ["memo"]
    );
}

/// A struct without any relation / schema-typed field: nothing is circular
/// and there is no FK relation. (A named struct's fields always have an
/// ident, so no "field without ident" case exists to test.)
#[test]
fn test_circular_fields_plain_struct_is_not_circular() {
    let analysis = analyze_circular_refs(
        &path(&["crate", "test"]),
        &[],
        r"pub struct Schema { pub id: i32, pub name: String, }",
    );
    assert_eq!(analysis.circular_fields, Vec::<String>::new());
    assert!(!analysis.has_fk_relations);
}

#[rstest]
#[case::plain_source_schema("MemoSchema", true)]
#[case::non_path_type("&str", false)]
fn type_target_detection_handles_non_relation_types(
    #[case] type_source: &str,
    #[case] expected: bool,
) {
    let ty: syn::Type = syn::parse_str(type_source).unwrap();
    assert_eq!(
        type_targets_source_schema(&ty, "memo", "MemoSchema"),
        expected
    );
}

#[test]
fn type_target_detection_rejects_empty_type_path() {
    let ty = syn::Type::Path(syn::TypePath {
        attrs: Vec::new(),
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::new(),
        },
    });

    assert!(!type_targets_source_schema(&ty, "memo", "MemoSchema"));
}
