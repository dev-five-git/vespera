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

mod adapter_ref;
mod derives;
mod unresolved;

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

/// The struct a `relation_adapters` entry names. The call-site file's
/// definition wins only when it derives `Schema` (the route-local DTO the user
/// listed, see [`choose_same_file`]); otherwise the crate-wide storage entry
/// is used, and the file's plain struct only when nothing is stored (R21-SM3-01
/// corrects the earlier "file first" wording). The storage is keyed by schema
/// name, so a same-named `Schema` in another module must not replace the
/// listed DTO (its `#[schema(name = ..)]` / serde renames would be lost).
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
        Some(definition) if stored.is_none() || derives::derives_schema(&definition) => Some(
            Cow::Owned(StructMetadata::new(struct_name.to_string(), definition)),
        ),
        // Cache hit: hand back a borrow so the (potentially large) struct
        // definition string is not cloned per lookup.
        _ => stored.map(Cow::Borrowed),
    }
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
            let cfg_serde = cfg_attr_serde(&field.attrs, |_, _| true);
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
fn cfg_attr_serde(
    attrs: &[syn::Attribute],
    mut keep: impl FnMut(&super::cfg_eval::Predicate, &syn::Meta) -> bool,
) -> Vec<TokenStream> {
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
                .filter(|item| keep(&predicate, item))
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
    let dto_ident = &dto_struct.ident;
    let attrs = rebuild_serde_attrs(&dto_struct.attrs, |meta| {
        !take_conversion_key(&mut serde, dto_ident, meta)
            && !matches!(serde_key(meta).as_str(), "into" | "bound" | "remote")
    });
    serde.proxy_attrs = attrs;
    // A `cfg_attr` conversion key is honoured when its predicate is statically
    // active (R34-SM3-L2). An undecided one never gets here: the adapter is
    // refused first (`adapter_ref::reject_undecided_cfg_attr`, SM35-3-L2).
    let cfg_serde = cfg_attr_serde(&dto_struct.attrs, |predicate, meta| {
        if super::cfg_eval::statically_active(predicate)
            && take_conversion_key(&mut serde, dto_ident, meta)
        {
            return false;
        }
        !matches!(
            serde_key(meta).as_str(),
            "from" | "try_from" | "default" | "into" | "bound" | "remote"
        )
    });
    serde.proxy_attrs.extend(cfg_serde);
    serde
}

/// Honours the conversion key `meta` names (`from` / `try_from` / `default`)
/// into `serde`: `true` when `meta` is one of them, whose code then replaces
/// the proxy's own.
fn take_conversion_key(serde: &mut DtoSerde, dto_ident: &syn::Ident, meta: &syn::Meta) -> bool {
    match serde_key(meta).as_str() {
        key @ ("from" | "try_from") => {
            if let Some(ty) = serde_value::<syn::Type>(meta) {
                serde.convert_from = Some((key == "try_from", ty));
            }
            true
        }
        "default" => {
            serde.default = Some(serde_value::<syn::ExprPath>(meta).map_or_else(
                || quote! { <#dto_ident as ::core::default::Default>::default() },
                |path| quote! { #path() },
            ));
            true
        }
        _ => false,
    }
}

fn explicit_adapter_struct(
    field_name: &str,
    adapter_name: &syn::Ident,
    schema_storage: &HashMap<String, StructMetadata>,
) -> syn::Result<(String, String, syn::ItemStruct)> {
    let dto_name = adapter_name.to_string();
    let Some(dto_meta) = find_same_file_struct_metadata(&dto_name, schema_storage) else {
        return Err(syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` explicitly requested adapter `{dto_name}`, but no same-file struct named `{dto_name}` was found"
            ),
        ));
    };
    // The wrapper names the call-site struct (`#dto_ident { .. }`). When the
    // stored `Schema` supplies the fields instead of a plain call-site struct
    // of that name, the expansion does not compile (E0560): say what to fix
    // (R21-SM3-02, round 21).
    // An underived call-site struct (chosen because the storage is empty) has no
    // component to `$ref`: the wrapper would dangle (R34-SM3-M1).
    let underived =
        matches!(dto_meta, Cow::Owned(_)) && !derives::derives_schema(&dto_meta.definition);
    if underived || same_file_definition(&dto_name).is_some_and(|file| file != dto_meta.definition)
    {
        return Err(syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` explicitly requested adapter `{dto_name}`, but the call-site struct `{dto_name}` does not derive `Schema`, so its fields would come from another `{dto_name}`; derive `Schema` on the call-site struct or rename the adapter"
            ),
        ));
    }

    let dto_struct = file_cache::parse_struct_cached(&dto_meta.definition).map_err(|e| {
        syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` adapter `{dto_name}`: its stored definition does not parse: {e}"
            ),
        )
    })?;
    adapter_ref::reject_generic_adapter(field_name, adapter_name, &dto_struct)?;
    adapter_ref::reject_undecided_cfg_attr(field_name, adapter_name, &dto_struct)?;
    let schema_ref_name = adapter_ref::schema_ref_name(
        matches!(dto_meta, Cow::Borrowed(_)).then_some(&*dto_meta),
        &dto_struct,
        &dto_name,
    );
    // A storage-fallback DTO (declared in another module) has its field types
    // and serde function paths copied verbatim into this call-site proxy: a
    // name only the DTO's own module resolves would not compile here
    // (R23-SM3-02).
    // Its fields must also be readable here: the wrapper builds the DTO with a
    // struct literal (R29-SM3-L2, checked with the names in `unresolved`).
    if same_file_definition(&dto_name).is_none() {
        unresolved::check_storage_fallback(
            field_name,
            adapter_name,
            &dto_name,
            &dto_struct,
            super::file_lookup::storage_module_path(&dto_meta).as_deref(),
        )?;
    }
    Ok((dto_name, schema_ref_name, dto_struct))
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
    let (dto_name, schema_ref_name, mut dto_struct) =
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
    // B6: $ref the adapter DTO's own schema component (`schema_ref_name`, see
    // `adapter_ref`), not the base relation schema.

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
mod tests;
