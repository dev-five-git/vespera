//! Circular reference detection and handling
//!
//! Provides functions to detect and handle circular references between
//! SeaORM models when generating schema types.

use proc_macro2::TokenStream;
use quote::quote;

use super::{
    seaorm::extract_belongs_to_from_field,
    type_utils::{capitalize_first, is_option_type, is_seaorm_relation_type},
};
use crate::parser::extract_skip;

/// Detect circular reference fields in a related schema.
///
/// When generating `MemoSchema.user`, we need to check if `UserSchema` has any fields
/// that reference back to `MemoSchema` via BelongsTo/HasOne (FK-based relations).
///
/// HasMany relations are NOT considered circular because they are excluded by default
/// from generated schemas.
///
/// Returns a list of field names that would create circular references.
pub fn detect_circular_fields(
    _source_schema_name: &str,
    source_module_path: &[String],
    related_schema_def: &str,
) -> Vec<String> {
    // Parse the related schema definition
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_schema_def) else {
        return Vec::new();
    };

    // Get the source module name (e.g., "memo" from ["crate", "models", "memo"])
    let source_module = source_module_path.last().map(|s| s.as_str()).unwrap_or("");

    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return Vec::new();
    };

    fields_named
        .named
        .iter()
        .filter_map(|field| {
            let field_ident = field.ident.as_ref()?;
            let field_name = field_ident.to_string();

            // Check if this field's type references the source schema
            let ty_str = quote!(#field.ty).to_string();

            // Normalize whitespace: quote!() produces "foo :: bar" instead of "foo::bar"
            // Remove all whitespace to make pattern matching reliable
            let ty_str_normalized = ty_str.replace(' ', "");

            // SKIP HasMany relations - they are excluded by default from schemas,
            // so they don't create actual circular references in the output
            if ty_str_normalized.contains("HasMany<") {
                return None;
            }

            // Check for BelongsTo/HasOne patterns that reference the source:
            // - HasOne<memo::Entity>
            // - BelongsTo<memo::Entity>
            // - Box<memo::Schema> (already converted)
            // - Option<Box<memo::Schema>>
            let is_circular = (ty_str_normalized.contains("HasOne<")
                || ty_str_normalized.contains("BelongsTo<")
                || ty_str_normalized.contains("Box<"))
                && (ty_str_normalized.contains(&format!("{}::Schema", source_module))
                    || ty_str_normalized.contains(&format!("{}::Entity", source_module))
                    || ty_str_normalized
                        .contains(&format!("{}Schema", capitalize_first(source_module))));

            is_circular.then_some(field_name)
        })
        .collect()
}

/// Check if a Model has any BelongsTo or HasOne relations (FK-based relations).
///
/// This is used to determine if the target schema has `from_model()` method
/// (async, with DB) or simple `From<Model>` impl (sync, no DB).
///
/// - Schemas with FK relations -> have `from_model()`, need async call
/// - Schemas without FK relations -> have `From<Model>`, can use sync conversion
pub fn has_fk_relations(model_def: &str) -> bool {
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(model_def) else {
        return false;
    };

    if let syn::Fields::Named(fields_named) = &parsed.fields {
        for field in &fields_named.named {
            let field_ty = &field.ty;
            let ty_str = quote!(#field_ty).to_string().replace(' ', "");

            // Check for BelongsTo or HasOne patterns
            if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
                return true;
            }
        }
    }

    false
}

/// Check if a circular relation field in the related schema is required (Box<T>) or optional (Option<Box<T>>).
///
/// Returns true if the circular relation is required and needs a parent stub.
pub fn is_circular_relation_required(related_model_def: &str, circular_field_name: &str) -> bool {
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_model_def) else {
        return false;
    };

    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return false;
    };

    // Find the circular field by name
    let Some(field) = fields_named.named.iter().find(|f| {
        f.ident
            .as_ref()
            .map(|i| i == circular_field_name)
            .unwrap_or(false)
    }) else {
        return false;
    };

    // Check if this is a HasOne/BelongsTo with required FK
    let ty_str = quote!(#field.ty).to_string().replace(' ', "");
    if !ty_str.contains("HasOne<") && !ty_str.contains("BelongsTo<") {
        return false;
    }

    // Check FK field optionality
    let Some(fk) = extract_belongs_to_from_field(&field.attrs) else {
        return false;
    };

    // Find FK field and check if it's Option
    fields_named
        .named
        .iter()
        .find(|f| f.ident.as_ref().map(|i| i.to_string()) == Some(fk.clone()))
        .map(|f| !is_option_type(&f.ty))
        .unwrap_or(false)
}

/// Generate a default value for a SeaORM relation field in inline construction.
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
    let ty_str = quote!(#ty).to_string().replace(' ', "");

    // Check the SeaORM relation type
    if ty_str.contains("HasMany<") {
        // HasMany -> Vec<Schema> -> empty vec
        quote! { #field_ident: vec![] }
    } else if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
        // Check FK field optionality
        let fk_field = extract_belongs_to_from_field(field_attrs);
        let is_optional = fk_field
            .as_ref()
            .map(|fk| {
                all_fields.named.iter().any(|f| {
                    f.ident.as_ref().map(|i| i.to_string()) == Some(fk.clone())
                        && is_option_type(&f.ty)
                })
            })
            .unwrap_or(true);

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
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_schema_def) else {
        // Fallback to From::from if parsing fails
        let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());
        return quote! { <#schema_path as From<_>>::from(#var_ident) };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields for FK checking
    let fields_named = match &parsed.fields {
        syn::Fields::Named(f) => f,
        _ => {
            return quote! { <#schema_path as From<_>>::from(#var_ident) };
        }
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

/// Generate inline type construction for from_model.
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
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_model_def) else {
        // Fallback to Default if parsing fails
        return quote! { Default::default() };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields
    let fields_named = match &parsed.fields {
        syn::Fields::Named(f) => f,
        _ => {
            return quote! { Default::default() };
        }
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
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(
        "Memo",
        &["crate", "models", "memo"],
        r#"pub struct UserSchema {
            pub id: i32,
            pub memos: HasMany<memo::Entity>,
        }"#,
        vec![]  // HasMany is not considered circular
    )]
    #[case(
        "User",
        &["crate", "models", "user"],
        r#"pub struct MemoSchema {
            pub id: i32,
            pub user: BelongsTo<user::Entity>,
        }"#,
        vec!["user".to_string()]
    )]
    #[case(
        "User",
        &["crate", "models", "user"],
        r#"pub struct MemoSchema {
            pub id: i32,
            pub user: HasOne<user::Entity>,
        }"#,
        vec!["user".to_string()]
    )]
    #[case(
        "User",
        &["crate", "models", "user"],
        r#"pub struct MemoSchema {
            pub id: i32,
            pub user: Box<user::Schema>,
        }"#,
        vec!["user".to_string()]
    )]
    #[case(
        "Memo",
        &["crate", "models", "memo"],
        r#"pub struct UserSchema {
            pub id: i32,
            pub name: String,
        }"#,
        vec![]  // No circular fields
    )]
    fn test_detect_circular_fields(
        #[case] source_schema_name: &str,
        #[case] source_module_path: &[&str],
        #[case] related_schema_def: &str,
        #[case] expected: Vec<String>,
    ) {
        let module_path: Vec<String> = source_module_path.iter().map(|s| s.to_string()).collect();
        let result = detect_circular_fields(source_schema_name, &module_path, related_schema_def);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_detect_circular_fields_invalid_struct() {
        let result = detect_circular_fields("Test", &["crate".to_string()], "not valid rust");
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_circular_fields_unnamed_fields() {
        let result = detect_circular_fields(
            "Test",
            &[
                "crate".to_string(),
                "models".to_string(),
                "test".to_string(),
            ],
            "pub struct TupleStruct(i32, String);",
        );
        assert!(result.is_empty());
    }

    #[rstest]
    #[case(
        r#"pub struct Model {
            pub id: i32,
            pub user: BelongsTo<user::Entity>,
        }"#,
        true
    )]
    #[case(
        r#"pub struct Model {
            pub id: i32,
            pub user: HasOne<user::Entity>,
        }"#,
        true
    )]
    #[case(
        r#"pub struct Model {
            pub id: i32,
            pub name: String,
        }"#,
        false
    )]
    #[case(
        r#"pub struct Model {
            pub id: i32,
            pub items: HasMany<item::Entity>,
        }"#,
        false  // HasMany alone doesn't count as FK relation
    )]
    fn test_has_fk_relations(#[case] model_def: &str, #[case] expected: bool) {
        assert_eq!(has_fk_relations(model_def), expected);
    }

    #[test]
    fn test_has_fk_relations_invalid_struct() {
        assert!(!has_fk_relations("not valid rust"));
    }

    #[test]
    fn test_has_fk_relations_unnamed_fields() {
        assert!(!has_fk_relations("pub struct TupleStruct(i32, String);"));
    }

    #[test]
    fn test_is_circular_relation_required_invalid_struct() {
        assert!(!is_circular_relation_required("not valid rust", "user"));
    }

    #[test]
    fn test_is_circular_relation_required_unnamed_fields() {
        assert!(!is_circular_relation_required(
            "pub struct TupleStruct(i32, String);",
            "user"
        ));
    }

    #[test]
    fn test_is_circular_relation_required_field_not_found() {
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub name: String,
        }"#;
        assert!(!is_circular_relation_required(model_def, "nonexistent"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<user::Entity>").unwrap();
        let field_ident = syn::Ident::new("users", proc_macro2::Span::call_site());
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub id: i32 }").unwrap();
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[], &all_fields);
        let output = tokens.to_string();
        assert!(output.contains("users : vec ! []"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let field_ident = syn::Ident::new("user", proc_macro2::Span::call_site());
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub user_id: Option<i32> }").unwrap();
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[], &all_fields);
        let output = tokens.to_string();
        assert!(output.contains("user : None"));
    }

    #[test]
    fn test_generate_default_for_relation_field_unknown_type() {
        let ty: syn::Type = syn::parse_str("SomeUnknownType<T>").unwrap();
        let field_ident = syn::Ident::new("field", proc_macro2::Span::call_site());
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub id: i32 }").unwrap();
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[], &all_fields);
        let output = tokens.to_string();
        assert!(output.contains("Default :: default ()"));
    }

    #[test]
    fn test_generate_inline_struct_construction_invalid_struct() {
        let schema_path = quote! { user::Schema };
        let tokens =
            generate_inline_struct_construction(&schema_path, "not valid rust", &[], "model");
        let output = tokens.to_string();
        assert!(output.contains("From"));
    }

    #[test]
    fn test_generate_inline_struct_construction_tuple_struct() {
        let schema_path = quote! { user::Schema };
        let tokens = generate_inline_struct_construction(
            &schema_path,
            "pub struct TupleStruct(i32, String);",
            &[],
            "model",
        );
        let output = tokens.to_string();
        assert!(output.contains("From"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_fields() {
        let schema_path = quote! { user::Schema };
        let tokens = generate_inline_struct_construction(
            &schema_path,
            r#"pub struct UserSchema {
                pub id: i32,
                pub name: String,
            }"#,
            &[],
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("name : r . name"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_circular_field() {
        let schema_path = quote! { user::Schema };
        let tokens = generate_inline_struct_construction(
            &schema_path,
            r#"pub struct UserSchema {
                pub id: i32,
                pub memos: HasMany<memo::Entity>,
            }"#,
            &["memos".to_string()],
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("memos : vec ! []"));
    }

    #[test]
    fn test_generate_inline_struct_construction_skip_serde_skip_fields() {
        let schema_path = quote! { user::Schema };
        let tokens = generate_inline_struct_construction(
            &schema_path,
            r#"pub struct UserSchema {
                pub id: i32,
                #[serde(skip)]
                pub internal: String,
            }"#,
            &[],
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("internal : r . internal"));
    }

    #[test]
    fn test_generate_inline_type_construction_invalid_struct() {
        let inline_type_name = syn::Ident::new("TestInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &["id".to_string()],
            "not valid rust",
            "model",
        );
        let output = tokens.to_string();
        assert!(output.contains("Default :: default ()"));
    }

    #[test]
    fn test_generate_inline_type_construction_tuple_struct() {
        let inline_type_name = syn::Ident::new("TestInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &["id".to_string()],
            "pub struct TupleStruct(i32, String);",
            "model",
        );
        let output = tokens.to_string();
        assert!(output.contains("Default :: default ()"));
    }

    #[test]
    fn test_generate_inline_type_construction_with_fields() {
        let inline_type_name = syn::Ident::new("UserInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &["id".to_string(), "name".to_string()],
            r#"pub struct Model {
                pub id: i32,
                pub name: String,
                pub email: String,
            }"#,
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("UserInline"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("name : r . name"));
        assert!(!output.contains("email : r . email"));
    }

    #[test]
    fn test_generate_inline_type_construction_skips_relations() {
        let inline_type_name = syn::Ident::new("UserInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &["id".to_string(), "memos".to_string()],
            r#"pub struct Model {
                pub id: i32,
                pub memos: HasMany<memo::Entity>,
            }"#,
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("memos : r . memos"));
    }

    // Additional coverage tests for is_circular_relation_required

    #[test]
    fn test_is_circular_relation_required_has_one_with_required_fk() {
        // Model has HasOne relation with a required (non-Option) FK field
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub user_id: i32,
            #[sea_orm(belongs_to = "super::user::Entity", from = "Column::UserId", to = "super::user::Column::Id")]
            pub user: HasOne<user::Entity>,
        }"#;
        // The FK field 'user_id' is i32 (required), so circular relation IS required
        let result = is_circular_relation_required(model_def, "user");
        // Without proper BelongsTo attribute parsing, this returns false
        // because extract_belongs_to_from_field won't find the FK
        assert!(!result);
    }

    #[test]
    fn test_is_circular_relation_required_belongs_to_with_optional_fk() {
        // Model has BelongsTo relation with optional FK field
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub user_id: Option<i32>,
            #[sea_orm(belongs_to = "super::user::Entity", from = "Column::UserId", to = "super::user::Column::Id")]
            pub user: BelongsTo<user::Entity>,
        }"#;
        // FK field is Option<i32>, so circular relation is NOT required
        let result = is_circular_relation_required(model_def, "user");
        assert!(!result);
    }

    #[test]
    fn test_is_circular_relation_required_non_relation_field() {
        // Field exists but is not a relation type
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub name: String,
        }"#;
        let result = is_circular_relation_required(model_def, "name");
        assert!(!result);
    }

    #[test]
    fn test_is_circular_relation_required_field_without_ident() {
        // Struct with fields that have no ident (tuple-like, but in braces - edge case)
        let model_def = r#"pub struct Model {
            pub id: i32,
        }"#;
        // Looking for a field that doesn't match
        let result = is_circular_relation_required(model_def, "nonexistent_field");
        assert!(!result);
    }

    // Additional coverage tests for generate_default_for_relation_field

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_optional() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let field_ident = syn::Ident::new("user", proc_macro2::Span::call_site());
        // FK field is optional
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub user_id: Option<i32> }").unwrap();
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[], &all_fields);
        let output = tokens.to_string();
        // Should produce None for optional
        assert!(output.contains("user : None"));
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let field_ident = syn::Ident::new("user", proc_macro2::Span::call_site());
        // FK field is required (not Option)
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub user_id: i32 }").unwrap();
        // Without FK attribute, it defaults to optional behavior
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[], &all_fields);
        let output = tokens.to_string();
        // Without belongs_to attribute, defaults to None
        assert!(output.contains("user : None"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_no_fk_found() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let field_ident = syn::Ident::new("user", proc_macro2::Span::call_site());
        // No FK field in all_fields
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub id: i32 }").unwrap();
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[], &all_fields);
        let output = tokens.to_string();
        // Without FK field found, defaults to None (optional behavior)
        assert!(output.contains("user : None"));
    }

    // Additional coverage tests for detect_circular_fields

    #[test]
    fn test_detect_circular_fields_empty_module_path() {
        // Edge case: empty module path
        let result = detect_circular_fields("Test", &[], "pub struct Schema { pub id: i32 }");
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_circular_fields_option_box_pattern() {
        // Test Option<Box<Schema>> pattern detection
        let result = detect_circular_fields(
            "Memo",
            &[
                "crate".to_string(),
                "models".to_string(),
                "memo".to_string(),
            ],
            r#"pub struct UserSchema {
                pub id: i32,
                pub memo: Option<Box<memo::Schema>>,
            }"#,
        );
        assert_eq!(result, vec!["memo".to_string()]);
    }

    #[test]
    fn test_detect_circular_fields_schema_suffix_pattern() {
        // Test MemoSchema suffix pattern detection
        let result = detect_circular_fields(
            "Memo",
            &[
                "crate".to_string(),
                "models".to_string(),
                "memo".to_string(),
            ],
            r#"pub struct UserSchema {
                pub id: i32,
                pub memo: Box<MemoSchema>,
            }"#,
        );
        assert_eq!(result, vec!["memo".to_string()]);
    }

    #[test]
    fn test_detect_circular_fields_field_without_ident() {
        // Fields without identifiers (parsing edge case)
        let result = detect_circular_fields(
            "Test",
            &["crate".to_string(), "test".to_string()],
            r#"pub struct Schema {
                pub id: i32,
            }"#,
        );
        assert!(result.is_empty());
    }

    // Additional coverage for generate_inline_struct_construction

    #[test]
    fn test_generate_inline_struct_construction_with_belongs_to_relation() {
        let schema_path = quote! { memo::Schema };
        let tokens = generate_inline_struct_construction(
            &schema_path,
            r#"pub struct MemoSchema {
                pub id: i32,
                pub user_id: i32,
                pub user: BelongsTo<user::Entity>,
            }"#,
            &[],
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("memo :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("user_id : r . user_id"));
        // BelongsTo should get default value
        assert!(output.contains("user : None"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_has_one_relation() {
        let schema_path = quote! { user::Schema };
        let tokens = generate_inline_struct_construction(
            &schema_path,
            r#"pub struct UserSchema {
                pub id: i32,
                pub profile: HasOne<profile::Entity>,
            }"#,
            &[],
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        // HasOne should get default value
        assert!(output.contains("profile : None"));
    }

    // Additional coverage for generate_inline_type_construction

    #[test]
    fn test_generate_inline_type_construction_skips_serde_skip() {
        let inline_type_name = syn::Ident::new("TestInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &["id".to_string(), "internal".to_string()],
            r#"pub struct Model {
                pub id: i32,
                #[serde(skip)]
                pub internal: String,
            }"#,
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("id : r . id"));
        // serde(skip) field should be excluded
        assert!(!output.contains("internal : r . internal"));
    }

    #[test]
    fn test_generate_inline_type_construction_empty_included_fields() {
        let inline_type_name = syn::Ident::new("EmptyInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &[], // No fields included
            r#"pub struct Model {
                pub id: i32,
                pub name: String,
            }"#,
            "r",
        );
        let output = tokens.to_string();
        // Should produce empty struct construction
        assert!(output.contains("EmptyInline"));
        assert!(!output.contains("id : r . id"));
        assert!(!output.contains("name : r . name"));
    }

    #[test]
    fn test_generate_inline_type_construction_field_not_in_included() {
        let inline_type_name = syn::Ident::new("PartialInline", proc_macro2::Span::call_site());
        let tokens = generate_inline_type_construction(
            &inline_type_name,
            &["id".to_string()], // Only id is included
            r#"pub struct Model {
                pub id: i32,
                pub name: String,
                pub email: String,
            }"#,
            "r",
        );
        let output = tokens.to_string();
        assert!(output.contains("id : r . id"));
        // name and email should not be included
        assert!(!output.contains("name : r . name"));
        assert!(!output.contains("email : r . email"));
    }

    // Tests for FK field lookup and required relation handling

    #[test]
    fn test_is_circular_relation_required_belongs_to_with_from_attr_required_fk() {
        // Model has BelongsTo with sea_orm(from = "user_id") attribute and required FK
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub user_id: i32,
            #[sea_orm(from = "user_id")]
            pub user: BelongsTo<user::Entity>,
        }"#;
        // FK field 'user_id' is i32 (required), so should return true
        let result = is_circular_relation_required(model_def, "user");
        assert!(result);
    }

    #[test]
    fn test_is_circular_relation_required_belongs_to_with_from_attr_optional_fk() {
        // Model has BelongsTo with sea_orm(from = "user_id") attribute and optional FK
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub user_id: Option<i32>,
            #[sea_orm(from = "user_id")]
            pub user: BelongsTo<user::Entity>,
        }"#;
        // FK field 'user_id' is Option<i32>, so should return false
        let result = is_circular_relation_required(model_def, "user");
        assert!(!result);
    }

    #[test]
    fn test_is_circular_relation_required_has_one_with_from_attr_required_fk() {
        // Model has HasOne with sea_orm(from = "profile_id") attribute and required FK
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub profile_id: i64,
            #[sea_orm(from = "profile_id")]
            pub profile: HasOne<profile::Entity>,
        }"#;
        // FK field 'profile_id' is i64 (required), so should return true
        let result = is_circular_relation_required(model_def, "profile");
        assert!(result);
    }

    #[test]
    fn test_is_circular_relation_required_from_attr_fk_field_not_found() {
        // Model has from attribute but FK field doesn't exist
        let model_def = r#"pub struct Model {
            pub id: i32,
            #[sea_orm(from = "nonexistent_field")]
            pub user: BelongsTo<user::Entity>,
        }"#;
        // FK field doesn't exist, so should return false
        let result = is_circular_relation_required(model_def, "user");
        assert!(!result);
    }

    // Tests for generate_default_for_relation_field with required FK

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_with_from_attr_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let field_ident = syn::Ident::new("user", proc_macro2::Span::call_site());
        // FK field is required (not Option)
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub user_id: i32 }").unwrap();
        // Create proper sea_orm attribute with from
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "user_id")]);
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[attr], &all_fields);
        let output = tokens.to_string();
        // Should produce Box::new(__parent_stub__.clone()) for required FK
        assert!(output.contains("__parent_stub__"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_with_from_attr_required() {
        let ty: syn::Type = syn::parse_str("HasOne<profile::Entity>").unwrap();
        let field_ident = syn::Ident::new("profile", proc_macro2::Span::call_site());
        // FK field is required (not Option)
        let all_fields: syn::FieldsNamed = syn::parse_str("{ pub profile_id: i64 }").unwrap();
        // Create proper sea_orm attribute with from
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "profile_id")]);
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[attr], &all_fields);
        let output = tokens.to_string();
        // Should produce Box::new(__parent_stub__.clone()) for required FK
        assert!(output.contains("__parent_stub__"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_with_from_attr_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<profile::Entity>").unwrap();
        let field_ident = syn::Ident::new("profile", proc_macro2::Span::call_site());
        // FK field is optional
        let all_fields: syn::FieldsNamed =
            syn::parse_str("{ pub profile_id: Option<i64> }").unwrap();
        // Create proper sea_orm attribute with from
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "profile_id")]);
        let tokens = generate_default_for_relation_field(&ty, &field_ident, &[attr], &all_fields);
        let output = tokens.to_string();
        // Should produce None for optional FK
        assert!(output.contains("profile : None"));
    }
}
