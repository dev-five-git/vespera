//! Source field attributes `schema_type!` carries onto the generated field
//! besides `serde` / `doc` (handled by `transformation`): `#[schema(..)]`
//! (OpenAPI constraints, descriptions, defaults — the derived `Validate`
//! follows them), `#[garde(..)]`, `#[deprecated]`, `#[cfg(..)]` and the
//! carriable part of `#[cfg_attr(..)]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;

use super::transformation::{reroot_serde_metas, serde_key};

/// What a source field contributes to the generated field.
#[derive(Default)]
pub struct CarriedAttrs {
    /// `#[schema(..)]` attributes (only emitted when `Schema` is derived).
    pub schema: Vec<syn::Attribute>,
    /// `#[garde(..)]`, `#[deprecated]`, `#[cfg(..)]` and filtered
    /// `#[cfg_attr(..)]` attributes, emitted as written.
    pub other: Vec<TokenStream>,
    /// `#[cfg_attr(pred, serde(..))]` items (paths re-rooted at the source
    /// module), emitted after `other`.
    pub cfg_serde: Vec<(syn::Meta, Vec<syn::Meta>)>,
    /// The `#[cfg(..)]` attributes alone: they also gate the field's
    /// assignment in the generated `From` / `from_model`.
    pub cfgs: Vec<syn::Attribute>,
    /// Any `#[garde(..)]` (the DTO then derives `garde::Validate`).
    pub garde: bool,
    /// Any `#[deprecated]` (the DTO's derives then read a deprecated field:
    /// the generated items `#[allow(deprecated)]`).
    pub deprecated: bool,
    /// A `#[schema(default ..)]` is present (the SeaORM-derived default
    /// must not add a second one).
    pub schema_default: bool,
}

/// Collects the carried attributes of one source field (defined in
/// `module_path`).
pub fn carried_field_attrs(
    attrs: &[syn::Attribute],
    schema_derived: bool,
    module_path: &[String],
) -> CarriedAttrs {
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
            carried.deprecated = true;
            carried.other.push(quote! { #attr });
        } else if path.is_ident("cfg") {
            carried.cfgs.push(attr.clone());
            carried.other.push(quote! { #attr });
        } else if path.is_ident("cfg_attr") {
            filtered_cfg_attr(attr, schema_derived, module_path, &mut carried);
        }
    }
    carried
}

/// `#[cfg_attr(pred, a, b)]` keeping only `schema` (when derived), `garde`,
/// `deprecated` and `doc` (into `other`) and `serde` (into `cfg_serde`,
/// without `rename`: the generated field's name is fixed by `schema_type!`,
/// a second `rename` would be a duplicate); nothing when it does not parse.
fn filtered_cfg_attr(
    attr: &syn::Attribute,
    schema_derived: bool,
    module_path: &[String],
    carried: &mut CarriedAttrs,
) {
    let Ok((predicate, metas)) = attr.parse_args_with(|input: syn::parse::ParseStream| {
        let predicate: syn::Meta = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let metas = Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated(input)?;
        Ok((predicate, metas))
    }) else {
        return;
    };
    let mut kept: Vec<&syn::Meta> = Vec::new();
    for meta in &metas {
        let path = meta.path();
        carried.garde |= path.is_ident("garde");
        carried.deprecated |= path.is_ident("deprecated");
        if path.is_ident("serde") {
            carried_cfg_serde(&predicate, meta, module_path, carried);
        } else if (schema_derived && path.is_ident("schema"))
            || path.is_ident("garde")
            || path.is_ident("deprecated")
            || path.is_ident("doc")
        {
            kept.push(meta);
        }
    }
    if !kept.is_empty() {
        carried
            .other
            .push(quote! { #[cfg_attr(#predicate, #(#kept),*)] });
    }
}

/// The items of a `serde(..)` inside `cfg_attr(predicate, ..)`.
fn carried_cfg_serde(
    predicate: &syn::Meta,
    meta: &syn::Meta,
    module_path: &[String],
    carried: &mut CarriedAttrs,
) {
    let Ok(list) = meta.require_list() else {
        return;
    };
    let Ok(items) = list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
    else {
        return;
    };
    let mut items: Vec<syn::Meta> = items
        .into_iter()
        .filter(|item| serde_key(item) != "rename")
        .collect();
    reroot_serde_metas(items.iter_mut(), module_path);
    if !items.is_empty() {
        carried.cfg_serde.push((predicate.clone(), items));
    }
}

/// One item of a `#[schema(..)]` list: its key and its tokens (a
/// `value_type = T` value is a type, every other value an expression).
struct SchemaItem {
    key: String,
    tokens: TokenStream,
    value_type: Option<syn::Type>,
}

/// The items of a `#[schema(..)]` attribute (`None` when it does not parse).
fn schema_items(attr: &syn::Attribute) -> Option<Vec<SchemaItem>> {
    let mut items = Vec::new();
    attr.parse_nested_meta(|meta| {
        let path = &meta.path;
        let key = path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        let mut value_type = None;
        let tokens = if meta.input.peek(syn::Token![=]) {
            let input = meta.value()?;
            if key == "value_type" {
                let ty: syn::Type = input.parse()?;
                let tokens = quote! { #path = #ty };
                value_type = Some(ty);
                tokens
            } else {
                let value: syn::Expr = input.parse()?;
                quote! { #path = #value }
            }
        } else if meta.input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in meta.input);
            let inner: TokenStream = content.parse()?;
            quote! { #path(#inner) }
        } else {
            quote! { #path }
        };
        items.push(SchemaItem {
            key,
            tokens,
            value_type,
        });
        Ok(())
    })
    .ok()?;
    Some(items)
}

impl CarriedAttrs {
    /// The carried `#[schema(..)]` attributes of a field `partial` wraps in
    /// `Option`: `default` is dropped (the field is `None` when absent — the
    /// source's serde default function is dropped too, so its documented
    /// default no longer holds) and `value_type = T` becomes
    /// `value_type = Option<T>`. An attribute left empty is removed; one
    /// that does not parse is kept as written. A `cfg_attr` serde codec /
    /// default no longer fits the `Option` field and is dropped; its
    /// `skip_serializing_if` predicate becomes `Option::is_none` (the
    /// source's predicate takes the unwrapped type).
    pub fn for_option_wrap(mut self) -> Self {
        self.schema = self
            .schema
            .into_iter()
            .filter_map(|attr| {
                let Some(items) = schema_items(&attr) else {
                    return Some(attr);
                };
                let kept: Vec<TokenStream> = items
                    .into_iter()
                    .filter(|item| item.key != "default")
                    .map(|item| match item.value_type {
                        Some(ty) => quote! { value_type = ::std::option::Option<#ty> },
                        None => item.tokens,
                    })
                    .collect();
                (!kept.is_empty()).then(|| syn::parse_quote! { #[schema(#(#kept),*)] })
            })
            .collect();
        for (_, items) in &mut self.cfg_serde {
            items.retain(|item| {
                !matches!(
                    serde_key(item).as_str(),
                    "with" | "serialize_with" | "deserialize_with" | "default"
                )
            });
            for item in items.iter_mut() {
                if serde_key(item) == "skip_serializing_if" {
                    *item = syn::parse_quote!(skip_serializing_if = "Option::is_none");
                }
            }
        }
        self.cfg_serde.retain(|(_, items)| !items.is_empty());
        self.schema_default = false;
        self
    }

    /// The `#[cfg_attr(pred, serde(..))]` attributes.
    pub fn cfg_serde_attrs(&self) -> Vec<TokenStream> {
        self.cfg_serde
            .iter()
            .map(|(predicate, items)| quote! { #[cfg_attr(#predicate, serde(#(#items),*))] })
            .collect()
    }
}

/// Whether a `#[schema(..)]` attribute sets `default`.
fn schema_has_default(attr: &syn::Attribute) -> bool {
    schema_items(attr).is_some_and(|items| items.iter().any(|item| item.key == "default"))
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
        let cfg_serde = carried.cfg_serde_attrs();
        quote! { #(#schema)* #(#other)* #(#cfg_serde)* }.to_string()
    }

    #[rstest]
    // `serde` inside `cfg_attr` is carried (paths the source module knows
    // are re-rooted; `dflt` is unknown here)
    // without `rename`; a non-list `serde` / unparseable list is
    // dropped.
    #[case::cfg_attr_serde(
        quote!(#[cfg_attr(feature = "x", serde(rename = "a", alias = "b", default = "dflt"), garde(skip))]),
        true,
        "# [cfg_attr (feature = \"x\" , garde (skip))] # [cfg_attr (feature = \"x\" , serde (alias = \"b\" , default = \"dflt\"))]"
    )]
    #[case::cfg_attr_serde_only_rename(
        quote!(#[cfg_attr(all(), serde(rename = "a"))]),
        true,
        ""
    )]
    #[case::cfg_attr_serde_not_list(quote!(#[cfg_attr(all(), serde)]), true, "")]
    #[case::cfg_attr_serde_unparseable(quote!(#[cfg_attr(all(), serde(= 1))]), true, "")]
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
        let module = ["crate", "m"].map(String::from);
        assert_eq!(
            render(&carried_field_attrs(&field(attrs), schema_derived, &module)),
            expected
        );
    }

    #[rstest]
    #[case::default_flag(quote!(#[schema(default)]), true)]
    #[case::default_value(quote!(#[schema(minimum = 1, default = 5)]), true)]
    #[case::list_then_default(quote!(#[schema(example(1), default = "x")]), true)]
    #[case::no_default(quote!(#[schema(minimum = 1)]), false)]
    // A type value (not an expression) before `default` used to stop the
    // walk.
    #[case::default_after_value_type(
        quote!(#[schema(value_type = Vec<String>, default = "x")]),
        true
    )]
    #[case::unparseable(quote!(#[schema(= 1)]), false)]
    fn detects_schema_default(#[case] attrs: TokenStream, #[case] expected: bool) {
        assert_eq!(
            carried_field_attrs(&field(attrs), true, &[]).schema_default,
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
    // A generic `value_type` (no expression) used to keep the attribute as
    // written: neither wrapped nor `default`-stripped.
    #[case::generic_value_type(
        quote!(#[schema(value_type = Vec<String>, default = "x", example(1), nullable)]),
        "# [schema (value_type = :: std :: option :: Option < Vec < String > > , example (1) , nullable)]"
    )]
    #[case::unparseable(quote!(#[schema(= 1)]), "# [schema (= 1)]")]
    // A `cfg_attr` serde codec / default no longer fits the `Option`.
    #[case::cfg_attr_serde_codec(
        quote!(#[cfg_attr(all(), serde(with = "c", alias = "b"))] #[cfg_attr(all(), serde(default))]),
        "# [cfg_attr (all () , serde (alias = \"b\"))]"
    )]
    // Its predicate took the unwrapped `String`: `Option::is_none` fits.
    #[case::cfg_attr_skip_serializing_if(
        quote!(#[cfg_attr(feature = "x", serde(skip_serializing_if = "String::is_empty"))]),
        "# [cfg_attr (feature = \"x\" , serde (skip_serializing_if = \"Option::is_none\"))]"
    )]
    fn option_wrap_attrs(#[case] attrs: TokenStream, #[case] expected: &str) {
        let carried = carried_field_attrs(&field(attrs), true, &[]).for_option_wrap();
        assert!(!carried.schema_default);
        assert_eq!(render(&carried), expected);
    }

    #[rstest]
    #[case::cfg_and_garde(quote!(#[cfg(test)] #[cfg_attr(all(), garde(skip))]), true, false)]
    #[case::deprecated(quote!(#[deprecated]), false, true)]
    #[case::cfg_attr_deprecated(quote!(#[cfg_attr(all(), deprecated)]), false, true)]
    fn flags(#[case] attrs: TokenStream, #[case] garde: bool, #[case] deprecated: bool) {
        let carried = carried_field_attrs(&field(attrs), true, &[]);
        assert_eq!((carried.garde, carried.deprecated), (garde, deprecated));
    }

    #[test]
    fn collects_cfgs() {
        let carried = carried_field_attrs(&field(quote!(#[cfg(test)])), true, &[]);
        assert_eq!(carried.cfgs.len(), 1);
    }
}
