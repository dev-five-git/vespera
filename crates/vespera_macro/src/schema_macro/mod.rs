//! Schema macro implementation
//!
//! Provides macros for generating `OpenAPI` schemas from struct types:
//! - `schema!` - Generate Schema value with optional field filtering
//! - `schema_type!` - Generate new struct type derived from existing type

pub mod cfg_eval;
mod circular;
mod codegen;
mod crate_layout;
mod defaults;
pub mod field_attrs;
mod field_selection;
pub mod file_cache;
mod file_lookup;
mod from_model;
mod garde_paths;
mod garde_peers;
mod generate_type;
mod inline_types;
mod input;
mod instantiate;
pub mod module_scope;
#[cfg(test)]
mod requested_variant_tests;
mod same_file_override;
mod seaorm;
mod transformation;
pub mod type_utils;
mod validation;

pub use file_cache::print_profile_summary;
#[cfg(test)]
pub use file_lookup::set_test_call_site_module;
pub use file_lookup::{
    call_site_module, call_site_module_snapshot, install_call_site_module, is_extern_crate,
    reexported_item, storage_module_path,
};
pub use generate_type::generate_schema_type_code;
pub use input::{SchemaInput, SchemaTypeInput};

use std::collections::{HashMap, HashSet};

use codegen::{generate_filtered_schema, validate_filter_names};
use proc_macro2::TokenStream;
use type_utils::extract_type_name;

use crate::metadata::StructMetadata;

/// The `#[derive(Schema)]` entry of the Rust type named `ident`.
///
/// Storage is keyed by the SCHEMA name, which `#[schema(name = "..")]` makes
/// differ from the Rust identifier: `#[schema(name = "UserDto")] struct User`
/// is stored under `UserDto` with `rust_ident = Some("User")`. A type is
/// named by its Rust identifier in `schema!` / `schema_type!`, so an entry
/// stored under `ident` only counts when it is not a rename of ANOTHER type
/// (`#[schema(name = "User")] struct UserRow` must not answer for `User`);
/// otherwise the single entry renamed from `ident` is used (several are
/// ambiguous: none).
pub fn storage_entry<'a>(
    storage: &'a HashMap<String, StructMetadata>,
    ident: &str,
) -> Option<&'a StructMetadata> {
    if let Some(meta) = storage.get(ident).filter(|meta| meta.rust_ident.is_none()) {
        return Some(meta);
    }
    let mut renamed = storage
        .values()
        .filter(|meta| meta.rust_ident.as_deref() == Some(ident));
    let first = renamed.next()?;
    renamed.next().is_none().then_some(first)
}

/// The stored item a `schema!` type path names among same-named items of
/// several modules: the variant of its qualified module (through a `pub
/// use` re-export there: `reexp::Page` read the last-derived variant), else
/// (a bare name) of the module defining the name in scope at the call site;
/// `None`: the storage entry `stored`.
///
/// # Errors
///
/// (L3, round 18: each used to document `stored` silently) The path goes
/// through another crate's import (`use other::types;` -> `types::Page`),
/// a bare name is imported from another crate, or a qualified path names a
/// module whose item is not the stored one (it has no `#[derive(Schema)]`).
fn requested_variant(
    ty: &syn::Type,
    stored: &StructMetadata,
) -> syn::Result<Option<StructMetadata>> {
    let name = extract_type_name(ty)?;
    let requested = file_lookup::requested_module_path(ty).map_err(|err| err.to_syn_error(ty))?;
    let qualified = requested.is_some();
    let Some(module) = requested.map_or_else(
        || bare_name_module(ty),
        |module| Some(reexported_item(&module, &name).map_or(module, |(defined_in, _)| defined_in)),
    ) else {
        return Ok(None);
    };
    if let Some(found) = crate::schema_impl::variants::in_module(&stored.name, &module) {
        return Ok(Some(found));
    }
    let defined_in = storage_module_path(stored);
    let other_crate = module.first().map(String::as_str) != Some("crate");
    // A bare name the call site imports explicitly (`use crate::a::Page;`) or
    // defines in its own module (`pub struct Page` without a derive) is named
    // as much as a qualified path is: the stored `Page` of another module is
    // not it (R20-SM3-03, round 20; R30-SM3-M1). A glob import stays lenient.
    let named_module = qualified || explicitly_named_at_call_site(&name);
    if other_crate
        || (named_module
            && defined_in
                .as_ref()
                .is_some_and(|defined| *defined != module))
    {
        let stored_at = defined_in.map_or_else(String::new, |defined| {
            format!(
                " (the `#[derive(Schema)]` `{name}` is in `{}`)",
                defined.join("::")
            )
        });
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "schema!: `{name}` of `{}` has no `#[derive(Schema)]` in this crate{stored_at}",
                module.join("::")
            ),
        ));
    }
    Ok(None)
}

/// The module defining the bare type name `ty` at the call site: the one a
/// call-site `use` imports it from (through `pub use` re-exports), else the
/// call site's own (a `use crate::first::Page;` used to be ignored, and the
/// last-derived `Page` of any module read).
fn bare_name_module(ty: &syn::Type) -> Option<Vec<String>> {
    let call_site = file_lookup::call_site_module()?;
    let name = extract_type_name(ty).ok()?;
    let module_scope::Resolved::Path(mut path) =
        module_scope::resolve_name(std::slice::from_ref(&name), &call_site)
    else {
        return Some(call_site);
    };
    let defined = path.pop()?;
    Some(reexported_item(&path, &defined).map_or(path, |(defined_in, _)| defined_in))
}

/// Whether the call site's module imports `name` with an explicit `use`
/// (glob imports are not in `ModuleScope::imports`) or defines it itself.
fn explicitly_named_at_call_site(name: &str) -> bool {
    file_lookup::call_site_module()
        .and_then(|call_site| module_scope::module_scope(&call_site))
        .is_some_and(|scope| scope.imports.contains_key(name) || scope.locals.contains(name))
}

/// Generate schema code from a struct with optional field filtering
pub fn generate_schema_code(
    input: &SchemaInput,
    schema_storage: &HashMap<String, StructMetadata>,
) -> Result<TokenStream, syn::Error> {
    // Extract type name from the Type
    let type_name = extract_type_name(&input.ty)?;

    // Find struct definition in storage (O(1) HashMap lookup)
    let struct_def = storage_entry(schema_storage, &type_name).ok_or_else(|| syn::Error::new_spanned(&input.ty, format!("type `{type_name}` not found. Make sure it has #[derive(Schema)] before this macro invocation")))?;
    // Same-named items of several modules: the one of the module the path
    // names (the storage keeps only the last derived).
    let variant = requested_variant(&input.ty, struct_def)?;
    let struct_def = variant.as_ref().unwrap_or(struct_def);

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = file_cache::parse_struct_cached(&struct_def.definition)
        .map_err(|e| {
            syn::Error::new_spanned(
                &input.ty,
                format!("failed to parse struct definition for `{type_name}`: {e}"),
            )
        })?;

    let parsed_struct = instantiate::instantiate(&parsed_struct, &input.ty, "schema!")?;
    validate_filter_names(&parsed_struct, input, &type_name)?;

    // Build omit set
    let omit_set: HashSet<String> = input.omit.iter().flatten().cloned().collect();

    // Build pick set
    let pick_set: HashSet<String> = input.pick.iter().flatten().cloned().collect();

    // Generate schema with filtering; the struct's field types resolve in
    // the module it is declared in.
    let source_module = storage_module_path(struct_def).unwrap_or_default();
    let schema_tokens = generate_filtered_schema(
        &parsed_struct,
        &omit_set,
        &pick_set,
        schema_storage,
        &struct_def.field_defaults,
        &source_module,
    );

    Ok(schema_tokens)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use quote::quote;

    use super::defaults::is_parseable_type;
    use super::same_file_override::maybe_generate_same_file_relation_override;
    use super::seaorm::RelationFieldInfo;
    use super::*;

    fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
        StructMetadata::new(name.to_string(), definition.to_string())
    }

    fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
        items.into_iter().map(|s| (s.name.clone(), s)).collect()
    }

    #[test]
    fn test_generate_schema_code_simple_struct() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(User);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
        assert!(output.contains("Schema"));
    }

    #[test]
    fn test_generate_schema_code_with_omit() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )]);

        let tokens = quote!(User, omit = ["password"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let output = generate_schema_code(&input, &storage).unwrap().to_string();

        assert!(output.contains("properties"));
        // The schema JSON is a string literal: its quotes are escaped.
        assert!(
            output.contains(r#"\"id\""#) && output.contains(r#"\"name\""#),
            "{output}"
        );
        assert!(!output.contains("password"), "omitted field kept: {output}");
    }

    #[test]
    fn test_generate_schema_code_with_pick() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub email: String }",
        )]);

        let tokens = quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let output = generate_schema_code(&input, &storage).unwrap().to_string();

        assert!(output.contains("properties"));
        assert!(
            output.contains(r#"\"id\""#) && output.contains(r#"\"name\""#),
            "{output}"
        );
        assert!(!output.contains("email"), "unpicked field kept: {output}");
    }

    fn renamed(schema_name: &str, ident: &str) -> StructMetadata {
        let mut meta = create_test_struct_metadata(
            schema_name,
            &format!(
                "#[schema(name = \"{schema_name}\")] pub struct {ident} {{ pub {ident}_field: i32 }}"
            ),
        );
        meta.rust_ident = Some(ident.to_string());
        meta
    }

    /// A type is found by its Rust identifier, not by its schema name: a
    /// renamed `User` (stored as `UserDto`) answers `User`, a `UserRow`
    /// renamed to `User` does not, and two types renamed from `User` are
    /// ambiguous.
    #[rstest::rstest]
    #[case::renamed_source(vec![renamed("UserDto", "User")], "User", Some("User_field"))]
    #[case::hijacking_rename(vec![renamed("User", "UserRow")], "User", None)]
    #[case::rename_target_is_not_an_ident(vec![renamed("UserDto", "User")], "UserDto", None)]
    #[case::plain_wins(vec![create_test_struct_metadata("User", "pub struct User { pub plain: i32 }"), renamed("Other", "User")], "User", Some("plain"))]
    #[case::ambiguous(vec![renamed("A", "User"), renamed("B", "User")], "User", None)]
    fn storage_entry_matches_the_rust_ident(
        #[case] items: Vec<StructMetadata>,
        #[case] ident: &str,
        #[case] field: Option<&str>,
    ) {
        let storage = to_storage(items);
        let input: SchemaInput = syn::parse_str(ident).unwrap();
        let result = generate_schema_code(&input, &storage);
        match field {
            Some(field) => assert!(result.unwrap().to_string().contains(field)),
            None => assert!(result.unwrap_err().to_string().contains("not found")),
        }
    }

    #[test]
    fn test_generate_schema_code_type_not_found() {
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let tokens = quote!(NonExistent);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_code_malformed_definition() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "BadStruct",
            "this is not valid rust code {{{",
        )]);

        let tokens = quote!(BadStruct);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"));
    }

    /// R27-SM3-L2: `pick` / `omit` naming a serde-skipped field is refused, as
    /// `schema!` refuses it, instead of carrying the field into the DTO.
    #[rstest::rstest]
    #[case::pick(quote!(NewUser from User, pick = ["hidden"]), "pick")]
    #[case::omit(quote!(NewUser from User, omit = ["hidden"]), "omit")]
    fn test_generate_schema_type_code_refuses_a_skipped_field(
        #[case] tokens: proc_macro2::TokenStream,
        #[case] param: &str,
    ) {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, #[serde(skip)] pub hidden: i32 }",
        )]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let err = generate_schema_type_code(&input, &storage)
            .unwrap_err()
            .to_string();
        assert!(err.contains(param), "{err}");
        assert!(err.contains("serde does not serialize it"), "{err}");
    }

    /// R27-SM3-L1 / round 27 contract: a JSON name shared by two fields is
    /// refused by `schema_type!` as by `schema!`.
    #[test]
    fn test_generate_schema_type_code_refuses_an_ambiguous_json_name() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { #[serde(rename = \"v\")] pub x: i32, #[serde(rename = \"v\")] pub y: i32 }",
        )]);
        let input: SchemaTypeInput = syn::parse2(quote!(NewUser from User, pick = ["v"])).unwrap();
        let err = generate_schema_type_code(&input, &storage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than one field"), "{err}");
    }

    /// The JSON name of a renamed field selects it (`schema!`'s rule).
    #[test]
    fn test_generate_schema_type_code_pick_accepts_a_json_name() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { #[serde(rename = \"userName\")] pub user_name: String, pub id: i32 }",
        )]);
        let input: SchemaTypeInput =
            syn::parse2(quote!(NewUser from User, pick = ["userName"])).unwrap();
        let tokens = generate_schema_type_code(&input, &storage)
            .expect("the JSON name selects the field")
            .0
            .to_string();
        assert!(tokens.contains("user_name"), "{tokens}");
        assert!(!tokens.contains("pub id"), "{tokens}");
    }

    #[test]
    fn test_generate_schema_type_code_pick_nonexistent_field() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(NewUser from User, pick = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_omit_nonexistent_field() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(NewUser from User, omit = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_rename_nonexistent_field() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(NewUser from User, rename = [("nonexistent", "new_name")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_type_not_found() {
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let tokens = quote!(NewUser from NonExistent);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_type_code_success() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(CreateUser from User, pick = ["name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("CreateUser"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_generate_schema_type_code_with_omit() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )]);

        let tokens = quote!(SafeUser from User, omit = ["password"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("SafeUser"));
        assert!(!output.contains("password"));
    }

    #[test]
    fn test_generate_schema_type_code_with_add() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("UserWithExtra"));
        assert!(output.contains("extra"));
    }

    #[test]
    fn test_generate_schema_type_code_relation_fields_can_be_omitted_and_readded_with_custom_types()
    {
        let storage = to_storage(vec![create_test_struct_metadata(
            "Model",
            r#"#[sea_orm(table_name = "article")]
            pub struct Model {
                pub id: i64,
                pub title: String,
                pub user: HasOne<super::user::Entity>,
                pub category: HasOne<super::category::Entity>,
                pub article_review_users: HasMany<super::article_review_user::Entity>
            }"#,
        )]);

        let tokens = quote!(
            ArticleResponse from Model,
            omit = ["user", "category", "article_review_users"],
            add = [
                ("user": Option<UserInArticle>),
                ("category": Option<CategoryInArticle>),
                ("article_review_users": Vec<ArticleReviewUserInArticle>)
            ]
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("pub user : Option < UserInArticle >"));
        assert!(output.contains("pub category : Option < CategoryInArticle >"));
        assert!(output.contains("pub article_review_users : Vec < ArticleReviewUserInArticle >"));
        assert!(!output.contains("Box < Schema >"));
        assert!(!output.contains("impl From"));
    }

    #[test]
    fn test_generate_schema_type_code_same_file_relation_adapters_when_explicit() {
        let storage = to_storage(vec![
            create_test_struct_metadata(
                "Model",
                r#"#[sea_orm(table_name = "article")]
                pub struct Model {
                    pub id: i64,
                    pub title: String,
                    pub user: HasOne<super::user::Entity>,
                    pub category: HasOne<super::category::Entity>,
                    pub article_review_users: HasMany<super::article_review_user::Entity>
                }"#,
            ),
            create_test_struct_metadata(
                "UserInArticle",
                "struct UserInArticle { pub id: i32, pub name: String }",
            ),
            create_test_struct_metadata(
                "CategoryInArticle",
                "struct CategoryInArticle { pub id: i64, pub name: String }",
            ),
        ]);

        let tokens = quote!(
            ArticleResponse from Model,
            relation_adapters = [("user", UserInArticle), ("category", CategoryInArticle)],
            add = [("article_review_users": Vec<ArticleReviewUserInArticle>)]
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("pub user : __VesperaArticleResponseUserRelation"));
        assert!(output.contains("pub category : __VesperaArticleResponseCategoryRelation"));
        assert!(output.contains("impl From < Option <"));
        assert!(output.contains("for __VesperaArticleResponseUserRelation"));
        assert!(output.contains("for __VesperaArticleResponseCategoryRelation"));
        assert!(output.contains("impl Clone for __VesperaArticleResponseUserRelation"));
        assert!(!output.contains("impl Clone for UserInArticle"));
        assert!(!output.contains("impl Clone for CategoryInArticle"));
    }

    /// The adapter DTO's metadata comes from `SCHEMA_STORAGE` with its
    /// derives stripped, so whether it derives `Clone` / `Deserialize` is
    /// unknowable: the helpers implement both on the generated wrapper and
    /// never on the DTO (re-implementing a derived trait is E0119).
    #[test]
    fn same_file_override_helpers_never_implement_traits_on_the_dto() {
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "HasOne".to_string(),
            schema_path: quote!(crate::models::user::Schema),
            is_optional: true,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            fk_optional: true,
            fk_to: None,
            via_rel: None,
            adapter: false,
        };
        // As `#[derive(Schema)]` stores it: no `#[derive(...)]` attribute.
        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            r#"#[serde(rename_all = "camelCase")]
            struct UserInArticle { pub id: i32, #[serde(default)] pub display_name: String }"#,
        )]);
        let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
        let adapter_name = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());

        let (override_field_ty, helper_tokens) = maybe_generate_same_file_relation_override(
            &new_type_name,
            "user",
            &adapter_name,
            &rel_info,
            &storage,
            true,
        )
        .expect("override generation should succeed");

        assert_eq!(
            override_field_ty.to_string(),
            "__VesperaArticleResponseUserRelation"
        );
        let file: syn::File = syn::parse2(helper_tokens).unwrap();
        insta::assert_snapshot!("same_file_override_helpers", prettyplease::unparse(&file));
    }

    #[test]
    fn test_generate_schema_type_code_generates_from_impl() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(UserResponse from User, pick = ["id", "name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("impl From"));
        assert!(output.contains("for UserResponse"));
    }

    #[test]
    fn test_generate_schema_type_code_no_from_impl_with_add() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(
            output.contains("UserWithExtra"),
            "expected struct UserWithExtra in output: {output}"
        );
        assert!(
            !output.contains("impl From"),
            "expected no From impl when `add` is used: {output}"
        );
    }

    // ========================
    // is_parseable_type tests
    // ========================

    #[test]
    fn test_is_parseable_type_primitives() {
        for ty_str in &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "String", "Decimal",
        ] {
            let ty: syn::Type = syn::parse_str(ty_str).unwrap();
            assert!(is_parseable_type(&ty, &[]), "{ty_str} should be parseable");
        }
    }

    #[test]
    fn test_is_parseable_type_non_parseable() {
        let ty: syn::Type = syn::parse_str("MyEnum").unwrap();
        assert!(!is_parseable_type(&ty, &[]));
    }

    #[test]
    fn test_is_parseable_type_non_path() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        assert!(!is_parseable_type(&ty, &[]));
    }
}
