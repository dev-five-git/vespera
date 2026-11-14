//! Source field attributes `schema_type!` carries onto the generated field
//! besides `serde` / `doc` (handled by `transformation`): `#[schema(..)]`
//! (OpenAPI constraints, descriptions, defaults — the derived `Validate`
//! follows them), `#[garde(..)]`, `#[deprecated]`, `#[cfg(..)]` and the
//! carriable part of `#[cfg_attr(..)]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;

/// What a source field contributes to the generated field.
#[derive(Default)]
pub struct CarriedAttrs {
    /// `#[schema(..)]` attributes (only emitted when `Schema` is derived).
    pub schema: Vec<syn::Attribute>,
    /// `#[garde(..)]`, `#[deprecated]`, `#[cfg(..)]` and filtered
    /// `#[cfg_attr(..)]` attributes, emitted as written.
    pub other: Vec<TokenStream>,
    /// The `#[cfg(..)]` attributes alone: they also gate the field's
    /// assignment in the generated `From` / `from_model`.
    pub cfgs: Vec<syn::Attribute>,
    /// Any `#[garde(..)]` (the DTO then derives `garde::Validate`).
    pub garde: bool,
    /// A `#[schema(default ..)]` is present (the SeaORM-derived default
    /// must not add a second one).
    pub schema_default: bool,
}

/// Collects the carried attributes of one source field.
pub fn carried_field_attrs(attrs: &[syn::Attribute], schema_derived: bool) -> CarriedAttrs {
    let mut carried = CarriedAttrs::default();
    for attr in attrs {
        let path = attr.path();
        if path.is_ident("schema") {
            carried.schema_default |= schema_has_default(attr);
            carried.schema.push(attr.clone());
        } else if path.is_ident("garde") {
            carried.garde = true;
            carried.other.push(quote! { #attr });
        } else if path.is_ident("deprecated") {
            carried.other.push(quote! { #attr });
        } else if path.is_ident("cfg") {
            carried.cfgs.push(attr.clone());
            carried.other.push(quote! { #attr });
        } else if path.is_ident("cfg_attr")
            && let Some(kept) = filtered_cfg_attr(attr, schema_derived, &mut carried)
        {
            carried.other.push(kept);
        }
    }
    carried
}

/// `#[cfg_attr(pred, a, b)]` keeping only `schema` (when derived), `garde`,
/// `deprecated` and `doc`; `None` when nothing is kept or it does not parse.
fn filtered_cfg_attr(
    attr: &syn::Attribute,
    schema_derived: bool,
    carried: &mut CarriedAttrs,
) -> Option<TokenStream> {
    let (predicate, metas) = attr
        .parse_args_with(|input: syn::parse::ParseStream| {
            let predicate: syn::Meta = input.parse()?;
            input.parse::<syn::Token![,]>()?;
            let metas = Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated(input)?;
            Ok((predicate, metas))
        })
        .ok()?;
    let kept: Vec<&syn::Meta> = metas
        .iter()
        .filter(|meta| {
            let path = meta.path();
            if path.is_ident("garde") {
                carried.garde = true;
            }
            (schema_derived && path.is_ident("schema"))
                || path.is_ident("garde")
                || path.is_ident("deprecated")
                || path.is_ident("doc")
        })
        .collect();
    (!kept.is_empty()).then(|| quote! { #[cfg_attr(#predicate, #(#kept),*)] })
}

impl CarriedAttrs {
    /// The carried `#[schema(..)]` attributes of a field `partial` wraps in
    /// `Option`: `default` is dropped (the field is `None` when absent — the
    /// source's serde default function is dropped too, so its documented
    /// default no longer holds) and `value_type = T` becomes
    /// `value_type = Option<T>`. An attribute left empty is removed; one
    /// that does not parse is kept as written.
    pub fn for_option_wrap(mut self) -> Self {
        self.schema = self
            .schema
            .into_iter()
            .filter_map(|attr| {
                let Ok(metas) =
                    attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                else {
                    return Some(attr);
                };
                let kept: Vec<syn::Meta> = metas
                    .into_iter()
                    .filter(|meta| !meta.path().is_ident("default"))
                    .map(|meta| match meta {
                        syn::Meta::NameValue(mut nv) if nv.path.is_ident("value_type") => {
                            let value = &nv.value;
                            // A type is no expression: carry it verbatim.
                            nv.value =
                                syn::Expr::Verbatim(quote! { ::std::option::Option<#value> });
                            syn::Meta::NameValue(nv)
                        }
                        other => other,
                    })
                    .collect();
                (!kept.is_empty()).then(|| syn::parse_quote! { #[schema(#(#kept),*)] })
            })
            .collect();
        self.schema_default = false;
        self
    }
}

/// Whether a `#[schema(..)]` attribute sets `default`.
fn schema_has_default(attr: &syn::Attribute) -> bool {
    let mut found = false;
    let _ = attr.parse_nested_meta(|meta| {
        found |= meta.path.is_ident("default");
        if meta.input.peek(syn::Token![=]) {
            meta.value()?.parse::<syn::Expr>()?;
        } else if meta.input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in meta.input);
            content.parse::<TokenStream>()?;
        }
        Ok(())
    });
    found
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[allow(clippy::needless_pass_by_value)]
    fn field(attrs: TokenStream) -> Vec<syn::Attribute> {
        let field: syn::Field = syn::parse::Parser::parse2(
            syn::Field::parse_named,
            quote! {
                #attrs pub value: i32
            },
        )
        .unwrap();
        field.attrs
    }

    fn render(carried: &CarriedAttrs) -> String {
        let schema = &carried.schema;
        let other = &carried.other;
        quote! { #(#schema)* #(#other)* }.to_string()
    }

    #[rstest]
    #[case::schema_constraint(quote!(#[schema(minimum = 1)]), true, "# [schema (minimum = 1)]")]
    #[case::garde(quote!(#[garde(range(min = 1))]), true, "# [garde (range (min = 1))]")]
    #[case::deprecated(quote!(#[deprecated = "old"]), true, "# [deprecated = \"old\"]")]
    #[case::cfg(quote!(#[cfg(feature = "x")]), true, "# [cfg (feature = \"x\")]")]
    #[case::cfg_attr_schema(
        quote!(#[cfg_attr(feature = "x", schema(minimum = 1), sea_orm(indexed))]),
        true,
        "# [cfg_attr (feature = \"x\" , schema (minimum = 1))]"
    )]
    #[case::cfg_attr_schema_not_derived(
        quote!(#[cfg_attr(feature = "x", schema(minimum = 1))]),
        false,
        ""
    )]
    #[case::cfg_attr_all_kinds(
        quote!(#[cfg_attr(all(), garde(skip), deprecated, doc = "d")]),
        true,
        "# [cfg_attr (all () , garde (skip) , deprecated , doc = \"d\")]"
    )]
    #[case::cfg_attr_unparseable(quote!(#[cfg_attr(feature = "x")]), true, "")]
    #[case::dropped(quote!(#[sea_orm(primary_key)] #[serde(default)]), true, "")]
    fn carries_expected_attributes(
        #[case] attrs: TokenStream,
        #[case] schema_derived: bool,
        #[case] expected: &str,
    ) {
        assert_eq!(
            render(&carried_field_attrs(&field(attrs), schema_derived)),
            expected
        );
    }

    #[rstest]
    #[case::default_flag(quote!(#[schema(default)]), true)]
    #[case::default_value(quote!(#[schema(minimum = 1, default = 5)]), true)]
    #[case::list_then_default(quote!(#[schema(example(1), default = "x")]), true)]
    #[case::no_default(quote!(#[schema(minimum = 1)]), false)]
    fn detects_schema_default(#[case] attrs: TokenStream, #[case] expected: bool) {
        assert_eq!(
            carried_field_attrs(&field(attrs), true).schema_default,
            expected
        );
    }

    #[rstest]
    #[case::only_default(quote!(#[schema(default = "x")]), "")]
    #[case::mixed(
        quote!(#[schema(min_length = 1, default = "x")]),
        "# [schema (min_length = 1)]"
    )]
    #[case::no_default(quote!(#[schema(minimum = 1)]), "# [schema (minimum = 1)]")]
    #[case::value_type(
        quote!(#[schema(value_type = String)]),
        "# [schema (value_type = :: std :: option :: Option < String >)]"
    )]
    #[case::unparseable(quote!(#[schema(= 1)]), "# [schema (= 1)]")]
    fn option_wrap_attrs(#[case] attrs: TokenStream, #[case] expected: &str) {
        let carried = carried_field_attrs(&field(attrs), true).for_option_wrap();
        assert!(!carried.schema_default);
        assert_eq!(render(&carried), expected);
    }

    #[test]
    fn flags_garde_and_collects_cfgs() {
        let carried = carried_field_attrs(
            &field(quote!(#[cfg(test)] #[cfg_attr(all(), garde(skip))])),
            true,
        );
        assert!(carried.garde);
        assert_eq!(carried.cfgs.len(), 1);
    }
}
