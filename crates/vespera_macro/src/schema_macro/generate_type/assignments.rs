//! The `From` / container-default initialisers of a `schema_type!` DTO.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;

use crate::parser::strip_raw_prefix_owned;

/// The `new_field: source.field` initialisers building the DTO from a
/// `source` value (`Some(..)` around `partial`-wrapped fields; `None` for
/// them with `wrapped_none` — a multipart container default must leave an
/// absent `partial` field `None`, as serde does for the serde-mode DTO).
pub(super) fn source_field_assignments(
    field_mappings: &[(syn::Ident, syn::Ident, bool, bool)],
    field_cfgs: &HashMap<String, TokenStream>,
    wrapped_none: bool,
) -> Vec<TokenStream> {
    field_mappings
        .iter()
        .map(|(new_ident, source_ident, wrapped, _is_relation)| {
            let cfg = field_cfgs.get(&strip_raw_prefix_owned(source_ident.to_string()));
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
