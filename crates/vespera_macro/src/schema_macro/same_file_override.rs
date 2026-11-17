//! Same-file relation override: explicitly listed route-local DTOs replace
//! single-value relation schemas without changing handler construction code
//! (see README "Same-File Relation Adapters").

use std::borrow::Cow;
use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;

use super::field_attrs::field_cfgs;
use super::file_cache;
use super::seaorm::RelationFieldInfo;
use super::transformation::{rebuild_serde_attrs, serde_key};
use super::type_utils::snake_to_pascal_case;
use crate::metadata::StructMetadata;
#[cfg(test)]
pub(super) struct __VesperaSameFileLookupFixture {
    value: i32,
}

/// Same-file adapter fixture: the storage holds a different struct of this
/// name, which the adapter lookup must not pick.
#[cfg(test)]
pub(super) struct __VesperaAdapterFixture {
    id: i32,
    name: String,
}

/// The struct a `relation_adapters` entry names: the one defined in the
/// call-site file first (the route-local DTO the user listed), then the
/// crate-wide storage (for a call site without a file). The storage is keyed
/// by schema name, so a same-named `Schema` in another module used to win
/// over the listed DTO (its `#[schema(name = ..)]` / serde renames lost).
pub(super) fn find_same_file_struct_metadata<'a>(
    struct_name: &str,
    schema_storage: &'a HashMap<String, StructMetadata>,
) -> Option<Cow<'a, StructMetadata>> {
    choose_same_file(
        struct_name,
        same_file_definition(struct_name),
        super::storage_entry(schema_storage, struct_name),
    )
}

/// The file item wins over the stored entry only when it is a `Schema`
/// (the listed route-local DTO derives it). A plain struct of that name in
/// the file (not a DTO, e.g. a same-named helper) must not replace the
/// `Schema` the imports name in another module: the stored entry is used
/// then, and the file item only when nothing is stored (R20-SM3-01, round 20).
fn choose_same_file<'a>(
    struct_name: &str,
    file_definition: Option<String>,
    stored: Option<&'a StructMetadata>,
) -> Option<Cow<'a, StructMetadata>> {
    match file_definition {
        Some(definition) if stored.is_none() || derives_schema(&definition) => Some(Cow::Owned(
            StructMetadata::new(struct_name.to_string(), definition),
        )),
        // Cache hit: hand back a borrow so the (potentially large) struct
        // definition string is not cloned per lookup.
        _ => stored.map(Cow::Borrowed),
    }
}

/// Whether the struct `definition` has `Schema` in one of its `derive`s.
fn derives_schema(definition: &str) -> bool {
    let Ok(item) = file_cache::parse_struct_cached(definition) else {
        return false;
    };
    item.attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .any(|attr| {
            attr.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|paths| {
                paths
                    .iter()
                    .any(|path| path.segments.last().is_some_and(|s| s.ident == "Schema"))
            })
        })
}

/// The definition of `struct_name` in the macro call-site file.
fn same_file_definition(struct_name: &str) -> Option<String> {
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
    file_cache::get_struct_definition(&file_path?, struct_name)
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
    // Only the `Schema` SUFFIX names the DTO: `FooSchemaBarSchema` is the
    // DTO of `FooSchemaBarModel`, not `FooModelBarModel`.
    let ident = last.ident.to_string();
    let renamed = ident.strip_suffix("Schema").map_or_else(
        || ident.replace("Schema", "Model"),
        |stem| format!("{stem}Model"),
    );
    last.ident = syn::Ident::new(&renamed, last.ident.span());
    Some(syn::Type::Path(type_path))
}

/// The adapter DTO's named fields.
pub(super) fn named_fields(struct_item: &syn::ItemStruct) -> syn::Result<Vec<&syn::Field>> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Err(syn::Error::new_spanned(
            struct_item,
            "same-file relation override DTO must be a named-field struct",
        ));
    };
    Ok(fields_named.named.iter().collect())
}

/// `field: <source>.field[.clone()]` per DTO field, each behind the
/// field's own conditions ([`field_cfgs`]: a `cfg_attr(p, cfg(q))` too; a
/// field compiled out has no assignment).
fn field_assignments(
    fields: &[&syn::Field],
    source: &TokenStream,
    clone: bool,
) -> Vec<TokenStream> {
    let clone = clone.then(|| quote! { .clone() });
    fields
        .iter()
        .filter_map(|field| {
            let ident = field.ident.as_ref()?;
            let cfgs = field_cfgs(&field.attrs);
            Some(quote! { #(#cfgs)* #ident: #source.#ident #clone })
        })
        .collect()
}

/// The deserialize proxy's fields: the DTO's types with their `serde` and
/// `doc` attributes, their conditions ([`field_cfgs`]) and the `serde`
/// items of a `cfg_attr`.
fn proxy_fields(fields: &[&syn::Field]) -> Vec<TokenStream> {
    fields
        .iter()
        .filter_map(|field| {
            let ident = field.ident.as_ref()?;
            let ty = &field.ty;
            let attrs = field.attrs.iter().filter(|attr| {
                let path = attr.path();
                path.is_ident("serde") || path.is_ident("doc")
            });
            let cfgs = field_cfgs(&field.attrs);
            let cfg_serde = cfg_attr_serde(&field.attrs, |_| true);
            Some(quote! {
                #(#cfgs)*
                #(#attrs)*
                #(#cfg_serde)*
                #ident: #ty
            })
        })
        .collect()
}

/// `#[cfg_attr(pred, serde(..), ..)]` of `attrs` reduced to the `serde`
/// items `keep` accepts (`rename_all` / `rename` must reach the proxy: it
/// otherwise reads other names than the DTO writes).
fn cfg_attr_serde(attrs: &[syn::Attribute], keep: impl Fn(&syn::Meta) -> bool) -> Vec<TokenStream> {
    use syn::punctuated::Punctuated;
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg_attr"))
        .filter_map(|attr| {
            let (predicate, metas) = attr
                .parse_args_with(|input: syn::parse::ParseStream| {
                    let predicate: super::cfg_eval::Predicate = input.parse()?;
                    input.parse::<syn::Token![,]>()?;
                    Ok((
                        predicate,
                        Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated(input)?,
                    ))
                })
                .ok()?;
            let items: Vec<syn::Meta> = metas
                .iter()
                .filter(|meta| meta.path().is_ident("serde"))
                .filter_map(|meta| {
                    meta.require_list()
                        .ok()?
                        .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                        .ok()
                })
                .flatten()
                .filter(|item| keep(item))
                .collect();
            (!items.is_empty()).then(|| quote! { #[cfg_attr(#predicate, serde(#(#items),*))] })
        })
        .collect()
}

/// The string value of a `key = "..."` serde item, parsed as `T`.
fn serde_value<T: syn::parse::Parse>(meta: &syn::Meta) -> Option<T> {
    let syn::Meta::NameValue(syn::MetaNameValue {
        value:
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit),
                ..
            }),
        ..
    }) = meta
    else {
        return None;
    };
    lit.parse().ok()
}

/// How the DTO's container serde attributes shape the wrapper's
/// `Deserialize`.
#[derive(Default)]
struct DtoSerde {
    /// The container attributes the proxy repeats (renames, tags,
    /// `deny_unknown_fields`, ...).
    proxy_attrs: Vec<TokenStream>,
    /// `from = "T"` / `try_from = "T"`: deserialize `T`, then convert.
    convert_from: Option<(bool, syn::Type)>,
    /// `default` / `default = "path"`: the expression building the DTO's
    /// default.
    default: Option<TokenStream>,
}

/// Splits the DTO's container serde attributes: the conversion keys
/// (`from` / `try_from` / `default`) are honoured by the generated code;
/// the keys whose impls the proxy cannot have (`into`, `bound`, `remote`)
/// are dropped.
fn dto_serde(dto_struct: &syn::ItemStruct) -> DtoSerde {
    let mut serde = DtoSerde::default();
    let attrs = rebuild_serde_attrs(&dto_struct.attrs, |meta| match serde_key(meta).as_str() {
        key @ ("from" | "try_from") => {
            if let Some(ty) = serde_value::<syn::Type>(meta) {
                serde.convert_from = Some((key == "try_from", ty));
            }
            false
        }
        "default" => {
            let dto_ident = &dto_struct.ident;
            serde.default = Some(serde_value::<syn::ExprPath>(meta).map_or_else(
                || quote! { <#dto_ident as ::core::default::Default>::default() },
                |path| quote! { #path() },
            ));
            false
        }
        "into" | "bound" | "remote" => false,
        _ => true,
    });
    serde.proxy_attrs = attrs;
    // A cfg-gated conversion key cannot be honoured conditionally: dropped.
    serde
        .proxy_attrs
        .extend(cfg_attr_serde(&dto_struct.attrs, |meta| {
            !matches!(
                serde_key(meta).as_str(),
                "from" | "try_from" | "default" | "into" | "bound" | "remote"
            )
        }));
    serde
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

    let dto_struct = file_cache::parse_struct_cached(&dto_meta.definition).map_err(|e| {
        syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` adapter `{dto_name}`: its stored definition does not parse: {e}"
            ),
        )
    })?;
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

/// The wrapper's `Deserialize` body (and the proxy items it needs).
fn deserialize_items(
    dto_ident: &syn::Ident,
    proxy_ident: &syn::Ident,
    fields: &[&syn::Field],
    serde: &DtoSerde,
) -> (TokenStream, TokenStream) {
    if let Some((fallible, from_ty)) = &serde.convert_from {
        let read = quote! {
            let value = <Option<#from_ty> as serde::Deserialize>::deserialize(deserializer)?;
        };
        let body = if *fallible {
            quote! {
                #read
                value
                    .map(<#dto_ident as ::core::convert::TryFrom<#from_ty>>::try_from)
                    .transpose()
                    .map(Self)
                    .map_err(serde::de::Error::custom)
            }
        } else {
            quote! {
                #read
                Ok(Self(value.map(<#dto_ident as ::core::convert::From<#from_ty>>::from)))
            }
        };
        return (body, TokenStream::new());
    }
    let proxy_fields = proxy_fields(fields);
    let proxy_to_dto = field_assignments(fields, &quote! { proxy }, false);
    let proxy_attrs = &serde.proxy_attrs;
    // A container `default` fills the missing fields from the DTO's default.
    let default_items = serde.default.as_ref().map(|dto_default| {
        let from_default = field_assignments(fields, &quote! { dto }, false);
        quote! {
            impl ::core::default::Default for #proxy_ident {
                fn default() -> Self {
                    let dto: #dto_ident = #dto_default;
                    Self {
                        #(#from_default),*
                    }
                }
            }
        }
    });
    let default_attr = default_items
        .is_some()
        .then(|| quote! { #[serde(default)] });
    let proxy = quote! {
        #[derive(serde::Deserialize)]
        #(#proxy_attrs)*
        #default_attr
        struct #proxy_ident {
            #(#proxy_fields),*
        }

        #default_items
    };
    let body = quote! {
        let proxy = <Option<#proxy_ident> as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self(proxy.map(|proxy| #dto_ident {
            #(#proxy_to_dto),*
        })))
    };
    (body, proxy)
}

pub(super) fn maybe_generate_same_file_relation_override(
    new_type_name: &syn::Ident,
    field_name: &str,
    adapter_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    schema_storage: &HashMap<String, StructMetadata>,
    derive_clone: bool,
) -> syn::Result<(TokenStream, TokenStream)> {
    let (dto_name, mut dto_struct) =
        explicit_adapter_struct(field_name, adapter_name, schema_storage)?;
    let dto_ident = adapter_name;
    // The proxy repeats the DTO's serde paths and field types, where `Self`
    // is the proxy (`Self::default_name`: E0599): spelled with the DTO.
    super::instantiate::name_self_in_types(&mut dto_struct);
    super::transformation::name_self_in_paths(&mut dto_struct, &dto_ident.clone().into());
    let type_name = crate::parser::strip_raw_prefix_owned(new_type_name.to_string());
    let field_pascal = snake_to_pascal_case(field_name);
    let wrapper_ident = syn::Ident::new(
        &format!("__Vespera{type_name}{field_pascal}Relation"),
        proc_macro2::Span::call_site(),
    );
    let proxy_ident = syn::Ident::new(
        &format!("__Vespera{type_name}{field_pascal}Proxy"),
        proc_macro2::Span::call_site(),
    );
    // B6: $ref the adapter DTO's own schema component (honoring its
    // `#[schema(name = ...)]` override), not the base relation schema.
    let schema_ref_name = dto_schema_ref_name(&dto_struct, &dto_name);

    let dto_doc_attrs: Vec<_> = dto_struct
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .collect();

    let fields = named_fields(&dto_struct)?;
    let clone_assignments = field_assignments(&fields, &quote! { dto }, true);
    // The DTO derives `Clone` only with `clone = true` (the default): a
    // wrapper `Clone` would need every adapter field to be `Clone` (E0277).
    let clone_impl = derive_clone.then(|| {
        quote! {
            impl Clone for #wrapper_ident {
                fn clone(&self) -> Self {
                    Self(self.0.as_ref().map(|dto| #dto_ident {
                        #(#clone_assignments),*
                    }))
                }
            }
        }
    });
    let model_ty = explicit_adapter_model_type(field_name, adapter_name, &dto_name, rel_info)?;
    let from_model_assignments = field_assignments(&fields, &quote! { source }, true);
    let (deserialize_body, proxy_items) =
        deserialize_items(dto_ident, &proxy_ident, &fields, &dto_serde(&dto_struct));

    // Every impl is on the WRAPPER, never on the user's DTO: whether the DTO
    // derives `Clone` / `Deserialize` cannot be told reliably (a
    // `#[derive(Schema)]` storage entry has its derives stripped,
    // `#[derive(std::clone::Clone)]` / `serde::Deserialize` are paths), an
    // impl on a DTO that derives them is E0119, and so is a `From<Model>`
    // for a DTO two fields / two `schema_type!` calls adapt.
    let helpers = quote! {
        #(#dto_doc_attrs)*
        #[derive(serde::Serialize, vespera::Schema)]
        #[serde(transparent)]
        #[schema(ref = #schema_ref_name, nullable)]
        struct #wrapper_ident(pub Option<#dto_ident>);

        #clone_impl

        impl Default for #wrapper_ident {
            fn default() -> Self {
                Self(None)
            }
        }

        #proxy_items

        impl<'de> serde::Deserialize<'de> for #wrapper_ident {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                #deserialize_body
            }
        }

        impl From<Option<#model_ty>> for #wrapper_ident {
            fn from(source: Option<#model_ty>) -> Self {
                Self(source.map(|source| #dto_ident {
                    #(#from_model_assignments),*
                }))
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

    /// R20-SM3-01 (round 20): a plain (not `Schema`-derived) struct of the
    /// adapter's name in the call-site file does not replace the `Schema`
    /// the storage holds (the one the imports name); the file's own
    /// `__VesperaAdapterFixture` (`id`, `name`) is ignored here.
    #[test]
    fn explicit_adapter_keeps_the_stored_schema_over_an_underived_file_struct() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "__VesperaAdapterFixture",
            "struct __VesperaAdapterFixture { value: i32 }",
        )]);
        let ident = syn::Ident::new("__VesperaAdapterFixture", proc_macro2::Span::call_site());
        let (_, dto) = explicit_adapter_struct("author", &ident, &storage).unwrap();
        let fields: Vec<String> = named_fields(&dto)
            .unwrap()
            .iter()
            .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
            .collect();
        assert_eq!(fields, ["value"]);
    }

    /// The file's `Schema`-derived DTO beats a same-named stored entry, and
    /// a same-named file struct without `Schema` only fills an empty storage.
    #[rstest::rstest]
    #[case::derived_file_item_wins(
        Some("#[derive(Serialize, vespera::Schema)] struct Dto { id: i32 }"),
        "struct Dto { value: i32 }",
        "id"
    )]
    #[case::underived_file_item_yields(
        Some("#[derive(Serialize)] struct Dto { id: i32 }"),
        "struct Dto { value: i32 }",
        "value"
    )]
    #[case::unparseable_file_item_yields(
        Some("struct Dto {"),
        "struct Dto { value: i32 }",
        "value"
    )]
    fn file_definition_beats_storage_only_when_it_is_a_schema(
        #[case] file: Option<&str>,
        #[case] stored: &str,
        #[case] expected_field: &str,
    ) {
        let stored = create_test_struct_metadata("Dto", stored);
        let chosen = choose_same_file("Dto", file.map(str::to_string), Some(&stored)).unwrap();
        assert!(
            chosen.definition.contains(expected_field),
            "{}",
            chosen.definition
        );
    }

    #[test]
    fn underived_file_item_fills_an_empty_storage() {
        let chosen =
            choose_same_file("Dto", Some("struct Dto { id: i32 }".to_string()), None).unwrap();
        assert!(chosen.definition.contains("id"));
    }

    /// A name the call-site file does not define falls back to the storage.
    #[test]
    fn explicit_adapter_falls_back_to_storage_when_the_file_lacks_it() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "__VesperaStorageOnly",
            "struct __VesperaStorageOnly { value: i32 }",
        )]);
        let ident = syn::Ident::new("__VesperaStorageOnly", proc_macro2::Span::call_site());
        let (name, dto) = explicit_adapter_struct("author", &ident, &storage).unwrap();
        assert_eq!(name, "__VesperaStorageOnly");
        assert_eq!(dto.ident.to_string(), "__VesperaStorageOnly");
    }

    #[test]
    fn named_fields_rejects_tuple_structs() {
        let struct_item: syn::ItemStruct = syn::parse_str("struct TupleDto(String);").unwrap();
        let error = named_fields(&struct_item).unwrap_err();
        assert!(error.to_string().contains("named-field struct"));
    }

    fn user_relation() -> RelationFieldInfo {
        RelationFieldInfo {
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
        }
    }

    /// The helpers for adapter DTOs: every impl lives on the wrapper (no
    /// `impl From<Model> for Dto`, so one DTO can adapt several fields),
    /// `#[cfg]` fields keep their cfg on each assignment, and the container
    /// conversion keys shape the `Deserialize`.
    #[rstest::rstest]
    #[case::plain("adapter_plain", "struct UserInArticle { id: i32, name: String }")]
    #[case::cfg_field(
        "adapter_cfg_field",
        "struct UserInArticle { id: i32, #[cfg(feature = \"x\")] #[serde(rename = \"n\")] name: String }"
    )]
    #[case::container_from(
        "adapter_container_from",
        "#[serde(from = \"UserWire\", into = \"UserWire\", rename_all = \"camelCase\")] struct UserInArticle { id: i32 }"
    )]
    #[case::container_try_from(
        "adapter_container_try_from",
        "#[serde(try_from = \"UserWire\")] struct UserInArticle { id: i32 }"
    )]
    #[case::container_default(
        "adapter_container_default",
        "#[serde(default, bound = \"\", deny_unknown_fields)] struct UserInArticle { id: i32, #[cfg(test)] name: String }"
    )]
    #[case::container_default_path(
        "adapter_container_default_path",
        "#[serde(default = \"users::fallback\")] struct UserInArticle { id: i32 }"
    )]
    // L13 (round 12): `cfg_attr` serde items reach the proxy (a cfg-gated
    // conversion key is dropped, non-serde items and unparseable ones too).
    #[case::cfg_attr_serde(
        "adapter_cfg_attr_serde",
        "#[cfg_attr(feature = \"x\", serde(rename_all = \"camelCase\", from = \"W\"), schema(name = \"Q\"))] #[cfg_attr(all(), serde(default))] #[cfg_attr(feature)] struct UserInArticle { #[cfg_attr(feature = \"x\", serde(rename = \"uid\"), garde(skip))] user_id: i32, #[cfg_attr(all(), serde, schema(minimum = 1))] other: i32 }"
    )]
    // L11 (round 16): a `cfg_attr(p, cfg(q))` field gates its assignments
    // and its proxy field (both were unconditional: E0560).
    #[case::cfg_attr_cfg(
        "adapter_cfg_attr_cfg",
        "struct UserInArticle { id: i32, #[cfg_attr(feature = \"a\", cfg(feature = \"b\"), serde(rename = \"n\"))] name: String }"
    )]
    // L3 (round 17): `Self` in the DTO's serde paths and field types names
    // the DTO, not the proxy (`<Proxy>::default_name`: E0599).
    #[case::self_paths(
        "adapter_self_paths",
        "#[serde(default = \"Self::make\")] struct UserInArticle { #[serde(default = \"Self::default_name\")] name: String, next: Option<Box<Self>> }"
    )]
    fn adapter_helpers_snapshot(#[case] name: &str, #[case] definition: &str) {
        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            definition,
        )]);
        let (_, helpers) = maybe_generate_same_file_relation_override(
            &syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site()),
            "user",
            &syn::Ident::new("UserInArticle", proc_macro2::Span::call_site()),
            &user_relation(),
            &storage,
            true,
        )
        .unwrap();
        let file: syn::File = syn::parse2(helpers).unwrap();
        insta::assert_snapshot!(name, prettyplease::unparse(&file));
    }

    #[test]
    fn adapter_requires_named_fields() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            "struct UserInArticle(i32);",
        )]);
        let error = maybe_generate_same_file_relation_override(
            &syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site()),
            "user",
            &syn::Ident::new("UserInArticle", proc_macro2::Span::call_site()),
            &user_relation(),
            &storage,
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("named-field struct"));
    }

    /// `clone = false`: no wrapper `Clone` (a non-`Clone` adapter field would
    /// fail it); an unparseable stored definition names the adapter.
    #[test]
    fn adapter_clone_follows_derive_clone_and_parse_errors_name_the_adapter() {
        let generate = |definition: &str, derive_clone: bool| {
            let storage = to_storage(vec![create_test_struct_metadata(
                "UserInArticle",
                definition,
            )]);
            maybe_generate_same_file_relation_override(
                &syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site()),
                "user",
                &syn::Ident::new("UserInArticle", proc_macro2::Span::call_site()),
                &user_relation(),
                &storage,
                derive_clone,
            )
        };
        let definition = "struct UserInArticle { id: i32 }";
        let (_, with) = generate(definition, true).unwrap();
        let (_, without) = generate(definition, false).unwrap();
        assert!(with.to_string().contains("impl Clone for"));
        assert!(!without.to_string().contains("impl Clone for"));
        let error = generate("struct UserInArticle {", true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("`user`") && error.contains("`UserInArticle`"),
            "{error}"
        );
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
            fk_optional: true,
            fk_to: None,
            via_rel: None,
            adapter: false,
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
            true,
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
            fk_optional: true,
            fk_to: None,
            via_rel: None,
            adapter: false,
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
            true,
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
        // Only the suffix is renamed: an inner `Schema` of the ident stays.
        let ty3 = related_model_type_from_schema_path(&quote!(crate::FooSchemaBarSchema))
            .expect("valid schema path");
        assert_eq!(
            quote!(#ty3).to_string(),
            quote!(crate::FooSchemaBarModel).to_string(),
        );
        // No `Schema` suffix: the historical rename of every occurrence.
        let ty4 =
            related_model_type_from_schema_path(&quote!(crate::SchemaUser)).expect("valid path");
        assert_eq!(
            quote!(#ty4).to_string(),
            quote!(crate::ModelUser).to_string()
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
            fk_optional: true,
            fk_to: None,
            via_rel: None,
            adapter: false,
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
            true,
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
