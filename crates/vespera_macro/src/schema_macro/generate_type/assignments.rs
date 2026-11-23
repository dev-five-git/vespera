//! The `From` / container-default initialisers of a `schema_type!` DTO and
//! the per-field rules they depend on (visibility, `#[cfg]` gating).

use proc_macro2::TokenStream;
use quote::quote;

use super::super::from_model::FieldMapping;

/// The `new_field: source.field` initialisers building the DTO from a
/// `source` value (`Some(..)` around `partial`-wrapped fields; `None` for
/// them with `wrapped_none` — a container default must leave an absent
/// `partial` field `None`, as serde does). Each repeats its OWN source
/// field's `#[cfg]`: `#[cfg]` twins share a name.
pub(super) fn source_field_assignments(
    field_mappings: &[FieldMapping],
    wrapped_none: bool,
) -> Vec<TokenStream> {
    field_mappings
        .iter()
        .map(|mapping| {
            let FieldMapping {
                new_ident,
                source_ident,
                wrapped,
                cfg,
                ..
            } = mapping;
            if *wrapped && wrapped_none {
                quote! { #cfg #new_ident: ::core::option::Option::None }
            } else if *wrapped {
                quote! { #cfg #new_ident: Some(source.#source_ident) }
            } else {
                quote! { #cfg #new_ident: source.#source_ident }
            }
        })
        .collect()
}

/// The field's conditions ([`super::super::field_attrs::field_cfgs`]).
pub(super) fn field_cfg(attrs: &[syn::Attribute]) -> TokenStream {
    let cfgs = super::super::field_attrs::field_cfgs(attrs);
    quote! { #(#cfgs)* }
}

/// Every item of `items` under the field's `cfg` (unchanged without one):
/// the helpers a field brings (SeaORM default fns, codec adapters, inline
/// relation types) exist only where the field does — a cfg-off field's
/// helper was dead code or named compiled-out items, and `#[cfg]` twins
/// emitted the same helper twice.
pub(super) fn gate_items(cfg: &TokenStream, items: TokenStream) -> TokenStream {
    if cfg.is_empty() {
        return items;
    }
    syn::parse2::<syn::File>(items.clone()).map_or_else(
        |_| quote! { #cfg #items },
        |file| {
            file.items
                .iter()
                .map(|item| quote! { #cfg #item })
                .collect()
        },
    )
}

/// The DTO field's visibility: a `pub(super)` / `pub(in path)` names a
/// module relative to (or above) the SOURCE, meaningless — or an error
/// (E0742) — at the call site, so it widens to `pub(crate)`.
pub(super) fn dto_visibility(vis: &syn::Visibility) -> TokenStream {
    match vis {
        syn::Visibility::Restricted(restricted)
            if !restricted.path.is_ident("crate") && !restricted.path.is_ident("self") =>
        {
            quote! { pub(crate) }
        }
        _ => quote! { #vis },
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::public("pub", "pub")]
    #[case::private("", "")]
    #[case::crate_visible("pub(crate)", "pub (crate)")]
    #[case::self_restricted("pub(self)", "pub (self)")]
    #[case::super_widened("pub(super)", "pub (crate)")]
    #[case::in_path_widened("pub(in crate::models)", "pub (crate)")]
    fn dto_visibility_widens_relative_scopes(#[case] vis: &str, #[case] expected: &str) {
        let vis: syn::Visibility = syn::parse_str(vis).unwrap();
        assert_eq!(dto_visibility(&vis).to_string(), expected);
    }

    #[test]
    fn gate_items_prefixes_every_item() {
        let cfg = quote! { #[cfg(feature = "a")] };
        let items = quote! { fn a() {} fn b() {} };
        assert_eq!(
            gate_items(&cfg, items.clone()).to_string(),
            "# [cfg (feature = \"a\")] fn a () { } # [cfg (feature = \"a\")] fn b () { }"
        );
        assert_eq!(
            gate_items(&TokenStream::new(), items.clone()).to_string(),
            items.to_string()
        );
        // Not items (never produced): the cfg still goes first.
        assert_eq!(
            gate_items(&cfg, quote! { 1 + }).to_string(),
            "# [cfg (feature = \"a\")] 1 +"
        );
    }
}
