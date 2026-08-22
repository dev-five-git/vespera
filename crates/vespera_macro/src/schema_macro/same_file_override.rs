//! Same-file relation override: explicitly listed route-local DTOs replace
//! single-value relation schemas without changing handler construction code
//! (see README "Same-File Relation Adapters").

use std::borrow::Cow;
use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;

use super::file_cache;
use super::seaorm::RelationFieldInfo;
use super::type_utils::snake_to_pascal_case;
use crate::metadata::StructMetadata;
#[cfg(test)]
pub(super) struct __VesperaSameFileLookupFixture {
    value: i32,
}

pub(super) fn find_same_file_struct_metadata<'a>(
    struct_name: &str,
    schema_storage: &'a HashMap<String, StructMetadata>,
) -> Option<Cow<'a, StructMetadata>> {
    // Cache hit: hand back a borrow so the (potentially large) struct
    // definition string is not cloned per lookup.  The fallback path
    // produces an owned `StructMetadata` from disk, so the unified return
    // type is `Cow<'_, StructMetadata>`.
    if let Some(metadata) = schema_storage.get(struct_name) {
        return Some(Cow::Borrowed(metadata));
    }

    let file_path = proc_macro2::Span::call_site().local_file();
    #[cfg(test)]
    let file_path = file_path.or_else(|| {
        Some(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("schema_macro")
                .join("same_file_override.rs"),
        )
    });
    let file_path = file_path?;
    let definition = file_cache::get_struct_definition(&file_path, struct_name)?;
    Some(Cow::Owned(StructMetadata::new(
        struct_name.to_string(),
        definition,
    )))
}

pub(super) fn related_model_type_from_schema_path(schema_path: &TokenStream) -> Option<syn::Type> {
    // Map a schema path (`…::UserSchema`) to its model path (`…::UserModel`)
    // by renaming ONLY the final path segment's identifier.  The previous
    // `to_string().replace("Schema", "Model")` rewrote EVERY "Schema"
    // substring in the whole path string, so a *module* segment that itself
    // contained "Schema" (e.g. `crate::SchemaStore::UserSchema`) was silently
    // corrupted into `crate::ModelStore::UserModel`, producing a dangling /
    // wrong `From<Model>` target.  Parsing to a `TypePath` and editing only
    // the last segment keeps every module segment verbatim and preserves any
    // generic arguments on the segment.
    let mut type_path: syn::TypePath = syn::parse2(schema_path.clone()).ok()?;
    let last = type_path.path.segments.last_mut()?;
    let renamed = last.ident.to_string().replace("Schema", "Model");
    last.ident = syn::Ident::new(&renamed, last.ident.span());
    Some(syn::Type::Path(type_path))
}

pub(super) fn has_derive(struct_item: &syn::ItemStruct, derive_name: &str) -> bool {
    struct_item.attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }

        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(derive_name) {
                found = true;
            }
            Ok(())
        });
        found
    })
}

pub(super) fn build_named_struct_field_assignments(
    struct_item: &syn::ItemStruct,
    source_expr: &TokenStream,
) -> syn::Result<Vec<TokenStream>> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Err(syn::Error::new_spanned(
            struct_item,
            "same-file relation override DTO must be a named-field struct",
        ));
    };

    let assignments = fields_named
        .named
        .iter()
        .filter_map(|field| {
            field.ident.as_ref().map(|ident| {
                quote! { #ident: #source_expr . #ident.clone() }
            })
        })
        .collect();

    Ok(assignments)
}

pub(super) fn build_proxy_fields(struct_item: &syn::ItemStruct) -> syn::Result<Vec<TokenStream>> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Err(syn::Error::new_spanned(
            struct_item,
            "same-file relation override DTO must be a named-field struct",
        ));
    };

    let fields = fields_named
        .named
        .iter()
        .filter_map(|field| {
            field.ident.as_ref().map(|ident| {
                let ty = &field.ty;
                let attrs: Vec<_> = field
                    .attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("serde") || attr.path().is_ident("doc"))
                    .collect();
                quote! {
                    #(#attrs)*
                    #ident: #ty
                }
            })
        })
        .collect();

    Ok(fields)
}

pub(super) fn build_proxy_to_dto_assignments(
    struct_item: &syn::ItemStruct,
) -> syn::Result<Vec<TokenStream>> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Err(syn::Error::new_spanned(
            struct_item,
            "same-file relation override DTO must be a named-field struct",
        ));
    };

    let assignments = fields_named
        .named
        .iter()
        .filter_map(|field| {
            field
                .ident
                .as_ref()
                .map(|ident| quote! { #ident: proxy.#ident })
        })
        .collect();

    Ok(assignments)
}

pub(super) fn build_clone_assignments(
    struct_item: &syn::ItemStruct,
) -> syn::Result<Vec<TokenStream>> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Err(syn::Error::new_spanned(
            struct_item,
            "same-file relation override DTO must be a named-field struct",
        ));
    };

    let assignments = fields_named
        .named
        .iter()
        .filter_map(|field| {
            field.ident.as_ref().map(|ident| {
                quote! { #ident: self.#ident.clone() }
            })
        })
        .collect();

    Ok(assignments)
}

/// The OpenAPI component name the adapter DTO is emitted under — its
/// `#[schema(name = "...")]` override when present, else the struct name.
fn dto_schema_ref_name(dto_struct: &syn::ItemStruct, dto_name: &str) -> String {
    crate::schema_impl::extract_schema_name_attr(&dto_struct.attrs)
        .unwrap_or_else(|| dto_name.to_string())
}

fn explicit_adapter_struct(
    field_name: &str,
    adapter_name: &syn::Ident,
    schema_storage: &HashMap<String, StructMetadata>,
) -> syn::Result<(String, syn::ItemStruct)> {
    let dto_name = adapter_name.to_string();
    let Some(dto_meta) = find_same_file_struct_metadata(&dto_name, schema_storage) else {
        return Err(syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` explicitly requested adapter `{dto_name}`, but no same-file struct named `{dto_name}` was found"
            ),
        ));
    };

    let dto_struct = file_cache::parse_struct_cached(&dto_meta.definition)
        .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e.to_string()))?;
    Ok((dto_name, dto_struct))
}

fn explicit_adapter_model_type(
    field_name: &str,
    adapter_name: &syn::Ident,
    dto_name: &str,
    rel_info: &RelationFieldInfo,
) -> syn::Result<syn::Type> {
    related_model_type_from_schema_path(&rel_info.schema_path).ok_or_else(|| syn::Error::new_spanned(adapter_name, format!("schema_type!: relation field `{field_name}` explicitly requested adapter `{dto_name}`, but the related model type could not be resolved from its schema path")))
}

pub(super) fn maybe_generate_same_file_relation_override(
    new_type_name: &syn::Ident,
    field_name: &str,
    adapter_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    schema_storage: &HashMap<String, StructMetadata>,
) -> syn::Result<(TokenStream, TokenStream)> {
    let (dto_name, dto_struct) = explicit_adapter_struct(field_name, adapter_name, schema_storage)?;
    let dto_ident = adapter_name;
    let wrapper_ident = syn::Ident::new(
        &format!(
            "__Vespera{}{}Relation",
            new_type_name,
            snake_to_pascal_case(field_name)
        ),
        proc_macro2::Span::call_site(),
    );
    let proxy_ident = syn::Ident::new(
        &format!(
            "__Vespera{}{}Proxy",
            new_type_name,
            snake_to_pascal_case(field_name)
        ),
        proc_macro2::Span::call_site(),
    );
    // B6: $ref the adapter DTO's own schema component (honoring its
    // `#[schema(name = ...)]` override), not the base relation schema.
    let schema_ref_name = dto_schema_ref_name(&dto_struct, &dto_name);

    let dto_serde_attrs: Vec<_> = dto_struct
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .collect();
    let dto_doc_attrs: Vec<_> = dto_struct
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .collect();

    let proxy_fields = build_proxy_fields(&dto_struct)?;
    let proxy_to_dto = build_proxy_to_dto_assignments(&dto_struct)?;
    let clone_assignments = build_clone_assignments(&dto_struct)?;
    let model_ty = explicit_adapter_model_type(field_name, adapter_name, &dto_name, rel_info)?;
    let source_expr = quote! { source };
    let from_model_assignments = build_named_struct_field_assignments(&dto_struct, &source_expr)?;

    // Coalesced helpers: previously three separate `quote!` invocations
    // and a `Vec<TokenStream>` accumulator were stitched together with
    // `#(#helper_tokens)*`.  We instead build the conditional Clone /
    // Deserialize sub-blocks as their own `TokenStream`s and splice
    // them into a single `quote!`, producing the same emitted Rust code
    // with one accumulator allocation removed.
    let clone_impl = if has_derive(&dto_struct, "Clone") {
        quote! {}
    } else {
        quote! {
            impl Clone for #dto_ident {
                fn clone(&self) -> Self {
                    Self {
                        #(#clone_assignments),*
                    }
                }
            }
        }
    };

    let deserialize_impl = if has_derive(&dto_struct, "Deserialize") {
        quote! {}
    } else {
        quote! {
            #[derive(serde::Deserialize)]
            #(#dto_serde_attrs)*
            struct #proxy_ident {
                #(#proxy_fields),*
            }

            impl<'de> serde::Deserialize<'de> for #dto_ident {
                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                where
                    D: serde::Deserializer<'de>,
                {
                    let proxy = #proxy_ident::deserialize(deserializer)?;
                    Ok(Self {
                        #(#proxy_to_dto),*
                    })
                }
            }
        }
    };

    let helpers = quote! {
        #clone_impl
        #deserialize_impl

        impl From<#model_ty> for #dto_ident {
            fn from(source: #model_ty) -> Self {
                Self {
                    #(#from_model_assignments),*
                }
            }
        }

        #(#dto_doc_attrs)*
        #[derive(serde::Serialize, serde::Deserialize, Clone, vespera::Schema)]
        #[serde(transparent)]
        #[schema(ref = #schema_ref_name, nullable)]
        struct #wrapper_ident(pub Option<#dto_ident>);

        impl From<Option<#model_ty>> for #wrapper_ident {
            fn from(source: Option<#model_ty>) -> Self {
                Self(source.map(Into::into))
            }
        }
    };

    Ok((quote! { #wrapper_ident }, helpers))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use quote::quote;

    use super::*;
    use crate::metadata::StructMetadata;
    use crate::schema_macro::seaorm::RelationFieldInfo;
    use crate::schema_macro::{SchemaTypeInput, generate_schema_type_code};

    fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
        StructMetadata::new(name.to_string(), definition.to_string())
    }

    fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
        items.into_iter().map(|s| (s.name.clone(), s)).collect()
    }

    #[test]
    fn test_find_same_file_struct_metadata_reads_test_fixture_from_current_module() {
        let storage: HashMap<String, StructMetadata> = HashMap::new();
        let metadata = find_same_file_struct_metadata("__VesperaSameFileLookupFixture", &storage)
            .expect("fixture should be found in schema_macro/same_file_override.rs");

        assert_eq!(metadata.name, "__VesperaSameFileLookupFixture");
        assert!(
            metadata
                .definition
                .contains("__VesperaSameFileLookupFixture")
        );
        assert!(metadata.definition.contains("value"));
    }

    #[test]
    fn test_has_derive_ignores_non_derive_attrs_and_detects_requested_derive() {
        let struct_item: syn::ItemStruct = syn::parse_str(
            r#"
        #[serde(rename_all = "camelCase")]
        #[derive(Clone, Debug)]
        struct Sample {
            value: i32,
        }
        "#,
        )
        .unwrap();

        assert!(has_derive(&struct_item, "Clone"));
        assert!(!has_derive(&struct_item, "Deserialize"));
    }

    #[test]
    fn test_build_named_struct_field_assignments_rejects_tuple_structs() {
        let struct_item: syn::ItemStruct = syn::parse_str("struct TupleDto(String);").unwrap();
        let source_expr = quote!(source);
        let error = build_named_struct_field_assignments(&struct_item, &source_expr).unwrap_err();
        assert!(error.to_string().contains("named-field struct"));
    }

    #[test]
    fn test_build_proxy_fields_rejects_tuple_structs() {
        let struct_item: syn::ItemStruct = syn::parse_str("struct TupleDto(String);").unwrap();
        let error = build_proxy_fields(&struct_item).unwrap_err();
        assert!(error.to_string().contains("named-field struct"));
    }

    #[test]
    fn test_build_proxy_to_dto_assignments_rejects_tuple_structs() {
        let struct_item: syn::ItemStruct = syn::parse_str("struct TupleDto(String);").unwrap();
        let error = build_proxy_to_dto_assignments(&struct_item).unwrap_err();
        assert!(error.to_string().contains("named-field struct"));
    }

    #[test]
    fn test_build_clone_assignments_rejects_tuple_structs() {
        let struct_item: syn::ItemStruct = syn::parse_str("struct TupleDto(String);").unwrap();
        let error = build_clone_assignments(&struct_item).unwrap_err();
        assert!(error.to_string().contains("named-field struct"));
    }

    #[test]
    fn test_maybe_generate_same_file_relation_override_errors_when_explicit_dto_is_missing() {
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "HasOne".to_string(),
            schema_path: quote!(crate::models::user::Schema),
            is_optional: true,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        let storage: HashMap<String, StructMetadata> = HashMap::new();
        let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
        let adapter_name = syn::Ident::new("MissingUserAdapter", proc_macro2::Span::call_site());

        let error = maybe_generate_same_file_relation_override(
            &new_type_name,
            "user",
            &adapter_name,
            &rel_info,
            &storage,
        )
        .unwrap_err();
        assert!(error.to_string().contains("MissingUserAdapter"));
        assert!(error.to_string().contains("relation field `user`"));
    }

    #[test]
    fn test_maybe_generate_same_file_relation_override_errors_for_invalid_model_type() {
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "HasOne".to_string(),
            schema_path: quote!(?),
            is_optional: true,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            "struct UserInArticle { id: i32 }",
        )]);
        let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
        let adapter_name = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());

        let error = maybe_generate_same_file_relation_override(
            &new_type_name,
            "user",
            &adapter_name,
            &rel_info,
            &storage,
        )
        .unwrap_err();
        assert!(error.to_string().contains("UserInArticle"));
        assert!(error.to_string().contains("related model type"));
    }

    #[test]
    fn related_model_type_renames_only_final_segment() {
        // A module segment that itself contains "Schema" (capital S) must be
        // left verbatim — only the trailing `…Schema` ident becomes `…Model`.
        // The previous `to_string().replace("Schema","Model")` corrupted
        // `SchemaStore` into `ModelStore`; this locks the regression.
        let ty = related_model_type_from_schema_path(&quote!(crate::SchemaStore::user::UserSchema))
            .expect("valid schema path resolves to a model type");
        assert_eq!(
            quote!(#ty).to_string(),
            quote!(crate::SchemaStore::user::UserModel).to_string(),
        );
        // A bare trailing `Schema` ident maps to `Model`.
        let ty2 = related_model_type_from_schema_path(&quote!(crate::models::user::Schema))
            .expect("valid schema path");
        assert_eq!(
            quote!(#ty2).to_string(),
            quote!(crate::models::user::Model).to_string(),
        );
        // A non-path token stream (e.g. a stray `?`) yields None, not a panic.
        assert!(related_model_type_from_schema_path(&quote!(?)).is_none());
    }

    #[test]
    fn test_generate_schema_type_code_normal_mode_relation_rename_and_custom_name() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "Model",
            r#"#[sea_orm(table_name = "articles")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub owner: HasOne<super::user::Entity>
            }"#,
        )]);

        let tokens = quote!(
            ArticleResponse from Model,
            name = "CustomArticleSchema",
            rename = [("name", "display_name")]
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("display_name"));
        assert!(output.contains("owner"));
        assert!(output.contains("Clone"));
        assert!(output.contains("CustomArticleSchema"));
        assert_eq!(metadata.unwrap().name, "CustomArticleSchema");
    }

    #[test]
    fn override_ref_honors_dto_schema_name_attribute() {
        // When the adapter DTO overrides its OpenAPI component name via
        // `#[schema(name = "...")]`, the generated wrapper's `#[schema(ref = ...)]`
        // must use that name (not the Rust struct name) so the emitted `$ref`
        // resolves instead of dangling.
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "HasOne".to_string(),
            schema_path: quote!(crate::models::user::Schema),
            is_optional: true,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            r#"#[schema(name = "ArticleUser")] struct UserInArticle { id: i32, name: String }"#,
        )]);
        let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
        let adapter_name = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());

        let (_field_ty, helpers) = maybe_generate_same_file_relation_override(
            &new_type_name,
            "user",
            &adapter_name,
            &rel_info,
            &storage,
        )
        .expect("override generation should succeed");

        let output = helpers.to_string();
        assert!(
            output.contains("ref = \"ArticleUser\""),
            "wrapper $ref must use the DTO's #[schema(name=...)] override, got: {output}"
        );
        assert!(
            !output.contains("ref = \"UserInArticle\""),
            "must not fall back to the struct name when a name override exists, got: {output}"
        );
    }
}
