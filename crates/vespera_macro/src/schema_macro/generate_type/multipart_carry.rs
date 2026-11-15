//! The source attributes a `schema_type!(..., multipart)` DTO keeps for the
//! `Multipart` derive, which honours serde's missing-field rules: a field's
//! `#[serde(default)]` / `default = "fn"` / `skip` / `skip_deserializing`,
//! the container's `#[serde(default)]` / `default = "fn"` and
//! `deny_unknown_fields`, and `#[try_from_multipart(..)]` (`strict`).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;

use super::super::transformation::reroot_serde_paths;

/// The serde metas of every `#[serde(..)]` in `attrs` (unparseable ones are
/// skipped: serde's own derive is not involved, the Multipart derive ignores
/// them).
fn serde_metas(attrs: &[syn::Attribute]) -> Vec<syn::Meta> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .filter_map(|attr| {
            attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .ok()
        })
        .flatten()
        .collect()
}

/// The field's missing-field keys — `default` (bare, or `= "fn"` re-rooted
/// at the source module), `skip`, `skip_deserializing` — and its `alias`es
/// (the derive accepts a part under them; they used to be dropped) as one
/// `#[serde(..)]`. `wrapped` (`partial` made the field `Option<T>`) drops
/// `default = "fn"`: it returns `T`, and a missing `Option` is `None` anyway.
pub(super) fn field_attr(
    attrs: &[syn::Attribute],
    module_path: &[String],
    wrapped: bool,
) -> TokenStream {
    let kept: Vec<syn::Meta> = serde_metas(&reroot_serde_paths(attrs, module_path))
        .into_iter()
        .filter(|meta| match meta {
            syn::Meta::Path(path) => ["default", "skip", "skip_deserializing"]
                .iter()
                .any(|key| path.is_ident(key)),
            syn::Meta::NameValue(nv) => {
                nv.path.is_ident("alias") || (!wrapped && nv.path.is_ident("default"))
            }
            syn::Meta::List(_) => false,
        })
        .collect();
    if kept.is_empty() {
        TokenStream::new()
    } else {
        quote! { #[serde(#(#kept),*)] }
    }
}

/// What a multipart DTO carries from the source container.
pub(super) struct ContainerCarry {
    /// `#[try_from_multipart(..)]` and `#[serde(default = "..")]`.
    pub(super) attrs: TokenStream,
    /// The DTO-default fn a container default points at.
    pub(super) helper: TokenStream,
}

/// The source container's `#[try_from_multipart(..)]` (verbatim) and its
/// default ([`container_default`]).
pub(super) fn container(
    source_attrs: &[syn::Attribute],
    module_path: &[String],
    source_type: &syn::Type,
    dto: &syn::Ident,
    assignments: Option<&[TokenStream]>,
) -> ContainerCarry {
    let strict = source_attrs
        .iter()
        .filter(|attr| attr.path().is_ident("try_from_multipart"));
    let default = container_default(source_attrs, module_path, source_type, dto, assignments);
    let default_attr = default.attrs;
    // The derive rejects unknown parts under it (and the schema is closed).
    let deny = crate::parser::extract_deny_unknown_fields(source_attrs)
        .then(|| quote! { #[serde(deny_unknown_fields)] });
    ContainerCarry {
        attrs: quote! { #(#strict)* #deny #default_attr },
        helper: default.helper,
    }
}

/// The source container's `#[serde(default)]` / `default = "fn"` (serde and
/// multipart DTOs alike): the DTO has no `Default` impl, so the default
/// becomes a generated fn building the DTO from the SOURCE's default
/// (`assignments` are the `field: source.field` initialisers). `None`
/// assignments (`add` fields, relations or fields the call site cannot read:
/// the DTO cannot be built from the source) drop the container default.
pub(super) fn container_default(
    source_attrs: &[syn::Attribute],
    module_path: &[String],
    source_type: &syn::Type,
    dto: &syn::Ident,
    assignments: Option<&[TokenStream]>,
) -> ContainerCarry {
    let source_default = serde_metas(&reroot_serde_paths(source_attrs, module_path))
        .into_iter()
        .find_map(|meta| match meta {
            syn::Meta::Path(path) if path.is_ident("default") => {
                Some(quote! { <#source_type as ::core::default::Default>::default() })
            }
            syn::Meta::NameValue(nv) if nv.path.is_ident("default") => match nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(lit),
                    ..
                }) => lit
                    .parse::<syn::ExprPath>()
                    .ok()
                    .map(|path| quote! { #path() }),
                _ => None,
            },
            _ => None,
        });
    let (attrs, helper) = match (source_default, assignments) {
        (Some(source_default), Some(assignments)) => {
            let helper_name = format_ident!(
                "__vespera_{}_default",
                crate::parser::strip_raw_prefix_owned(dto.to_string())
            );
            let helper_path = helper_name.to_string();
            (
                quote! { #[serde(default = #helper_path)] },
                quote! {
                    #[doc(hidden)]
                    #[allow(non_snake_case, deprecated, unused_variables)]
                    fn #helper_name() -> #dto {
                        let source: #source_type = #source_default;
                        #dto { #(#assignments),* }
                    }
                },
            )
        }
        _ => (TokenStream::new(), TokenStream::new()),
    };
    ContainerCarry { attrs, helper }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn attrs(tokens: &TokenStream) -> Vec<syn::Attribute> {
        let item: syn::ItemStruct = syn::parse2(quote! { #tokens struct S; }).unwrap();
        item.attrs
    }

    /// Only the missing-field keys survive; `default = "fn"` is dropped on a
    /// `partial`-wrapped field; lists and unparseable attributes are ignored.
    #[rstest]
    #[case::default_fn(quote!(#[serde(default = "f", rename = "x")]), false, "# [serde (default = \"f\")]")]
    #[case::default_fn_wrapped(quote!(#[serde(default = "f")]), true, "")]
    #[case::bare_default_wrapped(quote!(#[serde(default)]), true, "# [serde (default)]")]
    #[case::skips(quote!(#[serde(skip)] #[serde(skip_deserializing)]), false, "# [serde (skip , skip_deserializing)]")]
    #[case::list_and_others(quote!(#[serde(rename(deserialize = "a"), flatten)] #[doc = "d"]), false, "")]
    #[case::unparseable(quote!(#[serde(= nonsense)]), false, "")]
    #[case::alias(quote!(#[serde(alias = "a", default = "f")]), true, "# [serde (alias = \"a\")]")]
    fn field_attr_keeps_missing_field_keys(
        #[case] source: TokenStream,
        #[case] wrapped: bool,
        #[case] expected: &str,
    ) {
        assert_eq!(
            field_attr(&attrs(&source), &[], wrapped).to_string(),
            expected
        );
    }

    /// The container default becomes a DTO-building fn; `strict` is carried;
    /// no default, a non-string `default` value or no `assignments` (an
    /// `add` / relation DTO) leave the default out.
    #[rstest]
    #[case::trait_default("multipart_carry_container_trait", quote!(#[serde(default)] #[try_from_multipart(strict)]), true)]
    #[case::fn_default("multipart_carry_container_fn", quote!(#[serde(default = "make")]), true)]
    #[case::no_assignments("multipart_carry_container_no_assignments", quote!(#[serde(default)] #[try_from_multipart(strict)]), false)]
    #[case::non_string_default("multipart_carry_container_non_string", quote!(#[serde(default = 5)]), true)]
    #[case::no_default("multipart_carry_container_none", quote!(#[serde(rename_all = "camelCase")]), true)]
    #[case::deny_unknown_fields("multipart_carry_container_deny", quote!(#[serde(deny_unknown_fields)]), true)]
    fn container_carries_default_and_strict(
        #[case] name: &str,
        #[case] source: TokenStream,
        #[case] buildable: bool,
    ) {
        let source_type: syn::Type = syn::parse_quote!(crate::models::Upload);
        let dto: syn::Ident = syn::parse_quote!(UploadDto);
        let assignments = [quote! { title: source.title }];
        let carry = container(
            &attrs(&source),
            &[],
            &source_type,
            &dto,
            buildable.then_some(&assignments[..]),
        );
        let (attrs, helper) = (carry.attrs, carry.helper);
        insta::assert_snapshot!(name, format!("{attrs}\n{helper}"));
    }
}
