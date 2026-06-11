//! Circular reference detection and handling
//!
//! Provides functions to detect and handle circular references between
//! `SeaORM` models when generating schema types.

use std::collections::HashMap;

use super::type_utils::normalize_token_str;
use proc_macro2::TokenStream;
use quote::quote;

use super::{
    seaorm::extract_belongs_to_from_field,
    type_utils::{capitalize_first, is_option_type, is_seaorm_relation_type},
};
use crate::parser::extract_skip;

/// Combined result of circular reference analysis.
///
/// Produced by [`analyze_circular_refs()`] which parses a definition string once
/// and extracts all three pieces of information that would otherwise require
/// three separate parse calls.
#[derive(Clone)]
pub struct CircularAnalysis {
    /// Field names that would create circular references.
    pub circular_fields: Vec<String>,
    /// Whether the model has any `BelongsTo` or `HasOne` relations (FK-based).
    pub has_fk_relations: bool,
    /// For each `HasOne`/`BelongsTo` field, whether the FK is required (not `Option`).
    ///
    /// Keyed by field name. Contains entries for ALL `HasOne`/`BelongsTo` fields,
    /// not just circular ones, so callers can look up any relation field.
    pub circular_field_required: HashMap<String, bool>,
}

/// Analyze a struct definition for circular references, FK relations, and FK optionality
/// in a single parse + single field walk.
///
/// Parses the definition string once and extracts all circular reference
/// information in a single field walk.
pub fn analyze_circular_refs(source_module_path: &[String], definition: &str) -> CircularAnalysis {
    let Ok(parsed) = super::file_cache::parse_struct_cached(definition) else {
        return CircularAnalysis {
            circular_fields: Vec::new(),
            has_fk_relations: false,
            circular_field_required: HashMap::new(),
        };
    };

    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return CircularAnalysis {
            circular_fields: Vec::new(),
            has_fk_relations: false,
            circular_field_required: HashMap::new(),
        };
    };

    let source_module = source_module_path
        .last()
        .map_or("", std::string::String::as_str);

    let mut circular_fields = Vec::with_capacity(fields_named.named.len());
    let mut has_fk = false;
    let mut circular_field_required = HashMap::with_capacity(fields_named.named.len());

    // Pre-build field name → &Field index for O(1) FK column lookup
    // instead of O(N) linear search per FK relation
    let field_by_name: HashMap<String, &syn::Field> = fields_named
        .named
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|id| (id.to_string(), f)))
        .collect();
    // Precompute format strings used for circular reference detection
    let schema_pattern = format!("{source_module}::Schema");
    let entity_pattern = format!("{source_module}::Entity");
    let capitalized_pattern = format!("{}Schema", capitalize_first(source_module));

    for field in &fields_named.named {
        // FieldsNamed guarantees all fields have identifiers
        let field_ident = field.ident.as_ref().expect("named field has ident");
        let field_name = field_ident.to_string();
        let ty_str = normalize_token_str(&quote!(#field.ty));

        // --- has_fk_relations logic ---
        if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
            has_fk = true;

            // --- is_circular_relation_required logic (for ALL FK fields) ---
            let required = extract_belongs_to_from_field(&field.attrs).is_some_and(|fk| {
                field_by_name
                    .get(&fk)
                    .is_some_and(|f| !is_option_type(&f.ty))
            });
            circular_field_required.insert(field_name.clone(), required);
        }

        // --- detect_circular_fields logic ---
        // Skip HasMany — they are excluded by default and don't create circular refs
        if !ty_str.contains("HasMany<") {
            let is_circular = (ty_str.contains("HasOne<")
                || ty_str.contains("BelongsTo<")
                || ty_str.contains("Box<"))
                && (ty_str.contains(&schema_pattern)
                    || ty_str.contains(&entity_pattern)
                    || ty_str.contains(&capitalized_pattern));

            if is_circular {
                circular_fields.push(field_name);
            }
        }
    }

    CircularAnalysis {
        circular_fields,
        has_fk_relations: has_fk,
        circular_field_required,
    }
}

/// Generate a default value for a `SeaORM` relation field in inline construction.
///
/// - `HasMany<T>` -> `vec![]`
/// - `HasOne<T>`/`BelongsTo<T>` with optional FK -> `None`
/// - `HasOne<T>`/`BelongsTo<T>` with required FK -> needs parent stub (handled separately)
pub fn generate_default_for_relation_field(
    ty: &syn::Type,
    field_ident: &syn::Ident,
    field_attrs: &[syn::Attribute],
    all_fields: &syn::FieldsNamed,
) -> TokenStream {
    let ty_str = normalize_token_str(&quote!(#ty));

    // Check the SeaORM relation type
    if ty_str.contains("HasMany<") {
        // HasMany -> Vec<Schema> -> empty vec
        quote! { #field_ident: vec![] }
    } else if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
        // Check FK field optionality
        let fk_field = extract_belongs_to_from_field(field_attrs);
        let is_optional = fk_field.as_ref().is_none_or(|fk| {
            all_fields.named.iter().any(|f| {
                f.ident.as_ref().map(std::string::ToString::to_string) == Some(fk.clone())
                    && is_option_type(&f.ty)
            })
        });

        if is_optional {
            // Option<Box<Schema>> -> None
            quote! { #field_ident: None }
        } else {
            // Box<Schema> (required) -> use __parent_stub__
            // This variable will be defined by the caller when needed
            quote! { #field_ident: Box::new(__parent_stub__.clone()) }
        }
    } else {
        // Unknown relation type - try Default::default()
        quote! { #field_ident: Default::default() }
    }
}

/// Generate inline struct construction for a related schema, excluding circular fields.
///
/// Instead of `<user::Schema as From<_>>::from(r)`, generates:
/// ```ignore
/// user::Schema {
///     id: r.id,
///     name: r.name,
///     memos: vec![], // circular field - use default
/// }
/// ```
pub fn generate_inline_struct_construction(
    schema_path: &TokenStream,
    related_schema_def: &str,
    circular_fields: &[String],
    var_name: &str,
) -> TokenStream {
    // Parse the related schema definition
    let Ok(parsed) = super::file_cache::parse_struct_cached(related_schema_def) else {
        // Fallback to From::from if parsing fails
        let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());
        return quote! { <#schema_path as From<_>>::from(#var_ident) };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields for FK checking
    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return quote! { <#schema_path as From<_>>::from(#var_ident) };
    };

    let field_assignments: Vec<TokenStream> = fields_named
        .named
        .iter()
        .filter_map(|field| {
            let field_ident = field.ident.as_ref()?;
            let field_name = field_ident.to_string();

            // Skip fields marked with serde(skip)
            if extract_skip(&field.attrs) {
                return None;
            }

            if circular_fields.contains(&field_name) || is_seaorm_relation_type(&field.ty) {
                // Circular field or relation field - generate appropriate default
                // based on the SeaORM relation type
                Some(generate_default_for_relation_field(
                    &field.ty,
                    field_ident,
                    &field.attrs,
                    fields_named,
                ))
            } else {
                // Regular field - copy from model
                Some(quote! { #field_ident: #var_ident.#field_ident })
            }
        })
        .collect();

    quote! {
        #schema_path {
            #(#field_assignments),*
        }
    }
}

/// Generate inline type construction for `from_model`.
///
/// When we have an inline type (e.g., `MemoResponseRel_User`), this function generates
/// the construction code that only includes the fields present in the inline type.
///
/// ```ignore
/// MemoResponseRel_User {
///     id: r.id,
///     name: r.name,
///     email: r.email,
///     // memos field is NOT included - it was excluded from inline type
/// }
/// ```
pub fn generate_inline_type_construction(
    inline_type_name: &syn::Ident,
    included_fields: &[String],
    related_model_def: &str,
    var_name: &str,
) -> TokenStream {
    // Parse the related model definition
    let Ok(parsed) = super::file_cache::parse_struct_cached(related_model_def) else {
        // Fallback to Default if parsing fails
        return quote! { Default::default() };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields
    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return quote! { Default::default() };
    };

    let field_assignments: Vec<TokenStream> = fields_named
        .named
        .iter()
        .filter_map(|field| {
            let field_ident = field.ident.as_ref()?;
            let field_name = field_ident.to_string();

            // Skip fields marked with serde(skip)
            if extract_skip(&field.attrs) {
                return None;
            }

            // Skip relation fields (they are not in the inline type)
            if is_seaorm_relation_type(&field.ty) {
                return None;
            }

            // Only include fields that are in the inline type's field list
            if included_fields.contains(&field_name) {
                // Regular field - copy from model
                Some(quote! { #field_ident: #var_ident.#field_ident })
            } else {
                // This field was excluded (circular reference or otherwise)
                None
            }
        })
        .collect();

    quote! {
        #inline_type_name {
            #(#field_assignments),*
        }
    }
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use rstest::rstest;

    use super::*;

    fn ident(name: &str) -> syn::Ident {
        syn::Ident::new(name, proc_macro2::Span::call_site())
    }

    fn fields(src: &str) -> syn::FieldsNamed {
        syn::parse_str(src).unwrap()
    }

    fn required(def: &str, field: &str) -> bool {
        analyze_circular_refs(&[], def)
            .circular_field_required
            .get(field)
            .copied()
            .unwrap_or(false)
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
        let module_path: Vec<String> = source_module_path.iter().map(ToString::to_string).collect();
        assert_eq!(
            analyze_circular_refs(&module_path, related_schema_def).circular_fields,
            expected
        );
    }

    #[test]
    fn test_detect_circular_fields_invalid_struct() {
        assert!(
            analyze_circular_refs(&["crate".to_string()], "not valid rust")
                .circular_fields
                .is_empty()
        );
    }

    #[test]
    fn test_detect_circular_fields_unnamed_fields() {
        let path = vec![
            "crate".to_string(),
            "models".to_string(),
            "test".to_string(),
        ];
        assert!(
            analyze_circular_refs(&path, "pub struct TupleStruct(i32, String);")
                .circular_fields
                .is_empty()
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
            analyze_circular_refs(&[], model_def).has_fk_relations,
            expected
        );
    }

    #[test]
    fn test_has_fk_relations_invalid_struct() {
        assert!(!analyze_circular_refs(&[], "not valid rust").has_fk_relations);
    }

    #[test]
    fn test_has_fk_relations_unnamed_fields() {
        assert!(
            !analyze_circular_refs(&[], "pub struct TupleStruct(i32, String);").has_fk_relations
        );
    }

    #[test]
    fn test_is_circular_relation_required_invalid_struct() {
        assert!(!required("not valid rust", "user"));
    }

    #[test]
    fn test_is_circular_relation_required_unnamed_fields() {
        assert!(!required("pub struct TupleStruct(i32, String);", "user"));
    }

    #[test]
    fn test_is_circular_relation_required_field_not_found() {
        assert!(!required(
            "pub struct Model { pub id: i32, pub name: String, }",
            "nonexistent"
        ));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("users"),
                &[],
                &fields("{ pub id: i32 }")
            )
            .to_string()
            .contains("users : vec ! []")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub user_id: Option<i32> }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_unknown_type() {
        let ty: syn::Type = syn::parse_str("SomeUnknownType<T>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("field"),
                &[],
                &fields("{ pub id: i32 }")
            )
            .to_string()
            .contains("Default :: default ()")
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_invalid_struct() {
        assert!(
            generate_inline_struct_construction(
                &quote! { user::Schema },
                "not valid rust",
                &[],
                "model"
            )
            .to_string()
            .contains("From")
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_tuple_struct() {
        assert!(
            generate_inline_struct_construction(
                &quote! { user::Schema },
                "pub struct TupleStruct(i32, String);",
                &[],
                "model"
            )
            .to_string()
            .contains("From")
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_with_fields() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, pub name: String, }",
            &[],
            "r",
        )
        .to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("name : r . name"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_circular_field() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, pub memos: HasMany<memo::Entity>, }",
            &["memos".to_string()],
            "r",
        )
        .to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("memos : vec ! []"));
    }

    #[test]
    fn test_generate_inline_struct_construction_skip_serde_skip_fields() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, #[serde(skip)] pub internal: String, }",
            &[],
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("internal : r . internal"));
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

    #[test]
    fn test_generate_inline_type_construction_with_fields() {
        let output = generate_inline_type_construction(
            &ident("UserInline"),
            &["id".to_string(), "name".to_string()],
            r"pub struct Model { pub id: i32, pub name: String, pub email: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("UserInline"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("name : r . name"));
        assert!(!output.contains("email : r . email"));
    }

    #[test]
    fn test_generate_inline_type_construction_skips_relations() {
        let output = generate_inline_type_construction(
            &ident("UserInline"),
            &["id".to_string(), "memos".to_string()],
            r"pub struct Model { pub id: i32, pub memos: HasMany<memo::Entity>, }",
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("memos : r . memos"));
    }

    #[test]
    fn test_circular_field_required_has_one_with_required_fk() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, pub user_id: i32, #[sea_orm(belongs_to = "super::user::Entity", from = "Column::UserId", to = "super::user::Column::Id")] pub user: HasOne<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_belongs_to_with_optional_fk() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, pub user_id: Option<i32>, #[sea_orm(belongs_to = "super::user::Entity", from = "Column::UserId", to = "super::user::Column::Id")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_non_relation_field() {
        assert!(!required(
            r"pub struct Model { pub id: i32, pub name: String, }",
            "name"
        ));
    }

    #[test]
    fn test_circular_field_required_field_without_ident() {
        assert!(!required(
            r"pub struct Model { pub id: i32, }",
            "nonexistent_field"
        ));
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_optional() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub user_id: Option<i32> }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub user_id: i32 }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_no_fk_found() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub id: i32 }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_circular_fields_empty_module_path() {
        assert!(
            analyze_circular_refs(&[], "pub struct Schema { pub id: i32 }")
                .circular_fields
                .is_empty()
        );
    }

    #[test]
    fn test_circular_fields_option_box_pattern() {
        let path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        assert_eq!(
            analyze_circular_refs(
                &path,
                r"pub struct UserSchema { pub id: i32, pub memo: Option<Box<memo::Schema>>, }"
            )
            .circular_fields,
            vec!["memo".to_string()]
        );
    }

    #[test]
    fn test_circular_fields_schema_suffix_pattern() {
        let path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        assert_eq!(
            analyze_circular_refs(
                &path,
                r"pub struct UserSchema { pub id: i32, pub memo: Box<MemoSchema>, }"
            )
            .circular_fields,
            vec!["memo".to_string()]
        );
    }

    #[test]
    fn test_circular_fields_field_without_ident() {
        let path = vec!["crate".to_string(), "test".to_string()];
        assert!(
            analyze_circular_refs(&path, r"pub struct Schema { pub id: i32, }")
                .circular_fields
                .is_empty()
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_with_belongs_to_relation() {
        let output = generate_inline_struct_construction(&quote! { memo::Schema }, r"pub struct MemoSchema { pub id: i32, pub user_id: i32, pub user: BelongsTo<user::Entity>, }", &[], "r").to_string();
        assert!(output.contains("memo :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("user_id : r . user_id"));
        assert!(output.contains("user : None"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_has_one_relation() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, pub profile: HasOne<profile::Entity>, }",
            &[],
            "r",
        )
        .to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("profile : None"));
    }

    #[test]
    fn test_generate_inline_type_construction_skips_serde_skip() {
        let output = generate_inline_type_construction(
            &ident("TestInline"),
            &["id".to_string(), "internal".to_string()],
            r"pub struct Model { pub id: i32, #[serde(skip)] pub internal: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("internal : r . internal"));
    }

    #[test]
    fn test_generate_inline_type_construction_empty_included_fields() {
        let output = generate_inline_type_construction(
            &ident("EmptyInline"),
            &[],
            r"pub struct Model { pub id: i32, pub name: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("EmptyInline"));
        assert!(!output.contains("id : r . id"));
        assert!(!output.contains("name : r . name"));
    }

    #[test]
    fn test_generate_inline_type_construction_field_not_in_included() {
        let output = generate_inline_type_construction(
            &ident("PartialInline"),
            &["id".to_string()],
            r"pub struct Model { pub id: i32, pub name: String, pub email: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("name : r . name"));
        assert!(!output.contains("email : r . email"));
    }

    #[test]
    fn test_circular_field_required_belongs_to_with_from_attr_required_fk() {
        assert!(required(
            r#"pub struct Model { pub id: i32, pub user_id: i32, #[sea_orm(from = "user_id")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_belongs_to_with_from_attr_optional_fk() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, pub user_id: Option<i32>, #[sea_orm(from = "user_id")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_has_one_with_from_attr_required_fk() {
        assert!(required(
            r#"pub struct Model { pub id: i32, pub profile_id: i64, #[sea_orm(from = "profile_id")] pub profile: HasOne<profile::Entity>, }"#,
            "profile"
        ));
    }

    #[test]
    fn test_circular_field_required_from_attr_fk_field_not_found() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, #[sea_orm(from = "nonexistent_field")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_with_from_attr_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "user_id")]);
        let output = generate_default_for_relation_field(
            &ty,
            &ident("user"),
            &[attr],
            &fields("{ pub user_id: i32 }"),
        )
        .to_string();
        assert!(output.contains("__parent_stub__"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_with_from_attr_required() {
        let ty: syn::Type = syn::parse_str("HasOne<profile::Entity>").unwrap();
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "profile_id")]);
        let output = generate_default_for_relation_field(
            &ty,
            &ident("profile"),
            &[attr],
            &fields("{ pub profile_id: i64 }"),
        )
        .to_string();
        assert!(output.contains("__parent_stub__"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_with_from_attr_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<profile::Entity>").unwrap();
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "profile_id")]);
        let output = generate_default_for_relation_field(
            &ty,
            &ident("profile"),
            &[attr],
            &fields("{ pub profile_id: Option<i64> }"),
        )
        .to_string();
        assert!(output.contains("profile : None"));
    }
}
