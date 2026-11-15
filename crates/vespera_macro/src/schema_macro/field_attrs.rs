//! Source field attributes `schema_type!` carries onto the generated field
//! besides `serde` / `doc` (handled by `transformation`): `#[schema(..)]`
//! (OpenAPI constraints, descriptions, defaults — the derived `Validate`
//! follows them), `#[garde(..)]`, `#[deprecated]`, `#[cfg(..)]` and the
//! carriable part of `#[cfg_attr(..)]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;

use super::cfg_eval::Predicate;
use super::garde_paths::{reroot_garde_attr, reroot_garde_meta};
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
    pub cfg_serde: Vec<(Predicate, Vec<syn::Meta>)>,
    /// `#[cfg_attr(pred, schema(..))]` items, as `#[schema(..)]` attributes:
    /// `partial` rewrites them like [`Self::schema`]. Emitted only where
    /// `Schema` is derived; otherwise the registered metadata (which reads
    /// no `cfg_attr`) gets the statically active ones
    /// ([`Self::active_cfg_schema`]).
    pub cfg_schema: Vec<(Predicate, syn::Attribute)>,
    /// The `#[cfg(..)]` attributes alone: they also gate the field's
    /// assignment in the generated `From` / `from_model`.
    pub cfgs: Vec<syn::Attribute>,
    /// Any `#[garde(..)]` (the DTO then derives `garde::Validate`).
    pub garde: bool,
    /// Any `#[deprecated]` (the DTO's derives then read a deprecated field:
    /// the generated items `#[allow(deprecated)]`).
    pub deprecated: bool,
    /// An unconditional `#[schema(default ..)]` is present (the
    /// SeaORM-derived default must not add a second one).
    pub schema_default: bool,
    /// The predicates of `#[cfg_attr(pred, schema(default ..))]`: the
    /// SeaORM-derived default applies only where none of them holds.
    pub schema_default_cfgs: Vec<Predicate>,
}

/// Collects the carried attributes of one source field (defined in
/// `module_path`); `renamed`: the `rename` parameter renames the field (its
/// wire name is then pinned by `schema_type!`).
pub fn carried_field_attrs(
    attrs: &[syn::Attribute],
    schema_derived: bool,
    module_path: &[String],
    renamed: bool,
) -> CarriedAttrs {
    let mut carried = CarriedAttrs::default();
    // A `cfg_attr` serde `rename` is kept unless the field's name is already
    // fixed: a second `rename` would be a duplicate.
    let keep_rename = !renamed && !has_serde_rename(attrs);
    for attr in attrs {
        let path = attr.path();
        if path.is_ident("schema") {
            carried.schema_default |= schema_has_default(attr);
            carried.schema.push(attr.clone());
        } else if path.is_ident("garde") {
            carried.garde = true;
            let attr = reroot_garde_attr(attr, module_path);
            carried.other.push(quote! { #attr });
        } else if path.is_ident("deprecated") {
            carried.deprecated = true;
            carried.other.push(quote! { #attr });
        } else if path.is_ident("cfg") {
            carried.cfgs.push(attr.clone());
            carried.other.push(quote! { #attr });
        } else if path.is_ident("cfg_attr") {
            let context = (schema_derived, keep_rename, module_path);
            filtered_cfg_attr(attr, context, &mut carried);
        }
    }
    carried
}

/// Whether a top-level `#[serde(..)]` of the field sets `rename`.
fn has_serde_rename(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .filter_map(|attr| {
            attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .ok()
        })
        .any(|items| items.iter().any(|item| serde_key(item) == "rename"))
}

/// `#[cfg_attr(pred, a, b)]` keeping only `schema` (into `cfg_schema`),
/// `garde` (paths re-rooted), `deprecated` and `doc` (into `other`) and
/// `serde` (into `cfg_serde`, without `rename` unless `keep_rename`);
/// nothing when it does not parse.
fn filtered_cfg_attr(
    attr: &syn::Attribute,
    context: (bool, bool, &[String]),
    carried: &mut CarriedAttrs,
) {
    if let Ok((predicate, metas)) = attr.parse_args_with(parse_cfg_attr) {
        filtered_cfg_items(&predicate, &metas, context, carried);
    }
}

/// The `pred, a, b` of a `cfg_attr(..)`.
fn parse_cfg_attr(
    input: syn::parse::ParseStream<'_>,
) -> syn::Result<(Predicate, Punctuated<syn::Meta, syn::Token![,]>)> {
    let predicate: Predicate = input.parse()?;
    input.parse::<syn::Token![,]>()?;
    let metas = Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated(input)?;
    Ok((predicate, metas))
}

/// The items of one `cfg_attr(predicate, ..)` (see [`filtered_cfg_attr`]);
/// a nested `cfg_attr(inner, ..)` is flattened under `all(predicate,
/// inner)` (it used to be dropped).
fn filtered_cfg_items(
    predicate: &Predicate,
    metas: &Punctuated<syn::Meta, syn::Token![,]>,
    context: (bool, bool, &[String]),
    carried: &mut CarriedAttrs,
) {
    let (schema_derived, keep_rename, module_path) = context;
    let mut kept: Vec<syn::Meta> = Vec::new();
    for meta in metas {
        let path = meta.path();
        if path.is_ident("cfg_attr") {
            if let Ok((inner, metas)) = meta
                .require_list()
                .and_then(|list| list.parse_args_with(parse_cfg_attr))
            {
                let combined: Predicate = syn::parse_quote!(all(#predicate, #inner));
                filtered_cfg_items(&combined, &metas, context, carried);
            }
            continue;
        }
        carried.garde |= path.is_ident("garde");
        carried.deprecated |= path.is_ident("deprecated");
        if path.is_ident("serde") {
            carried_cfg_serde(predicate, meta, (keep_rename, module_path), carried);
        } else if path.is_ident("schema") {
            let attr: syn::Attribute = syn::parse_quote! { #[#meta] };
            // Gates the SeaORM-derived default of the emitted field only
            // (the metadata reads no `cfg_attr`).
            if schema_derived && schema_has_default(&attr) {
                carried.schema_default_cfgs.push(predicate.clone());
            }
            carried.cfg_schema.push((predicate.clone(), attr));
        } else if path.is_ident("garde") {
            kept.push(reroot_garde_meta(meta, module_path));
        } else if path.is_ident("deprecated") || path.is_ident("doc") {
            kept.push(meta.clone());
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
    predicate: &Predicate,
    meta: &syn::Meta,
    (keep_rename, module_path): (bool, &[String]),
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
        .filter(|item| keep_rename || serde_key(item) != "rename")
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
            .filter_map(option_wrap_schema)
            .collect();
        self.cfg_schema = self
            .cfg_schema
            .into_iter()
            .filter_map(|(predicate, attr)| Some((predicate, option_wrap_schema(attr)?)))
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
        self.schema_default_cfgs.clear();
        self
    }

    /// The predicates of the carried `#[cfg_attr(pred, serde(default ..))]`.
    pub fn cfg_serde_default_predicates(&self) -> Vec<Predicate> {
        self.cfg_serde
            .iter()
            .filter(|(_, items)| items.iter().any(|item| serde_key(item) == "default"))
            .map(|(predicate, _)| predicate.clone())
            .collect()
    }

    /// The `#[cfg_attr(pred, schema(..))]` attributes.
    pub fn cfg_schema_attrs(&self) -> Vec<TokenStream> {
        self.cfg_schema
            .iter()
            .map(|(predicate, attr)| {
                let meta = &attr.meta;
                quote! { #[cfg_attr(#predicate, #meta)] }
            })
            .collect()
    }

    /// The `#[schema(..)]` of the `cfg_attr`s whose predicate is known to
    /// hold (the metadata of an `ignore` type reads no `cfg_attr`; it used
    /// to drop them all).
    pub fn active_cfg_schema(&self) -> Vec<&syn::Attribute> {
        self.cfg_schema
            .iter()
            .filter(|(predicate, _)| super::cfg_eval::statically_active(predicate))
            .map(|(_, attr)| attr)
            .collect()
    }

    /// The `#[cfg_attr(pred, serde(..))]` attributes.
    pub fn cfg_serde_attrs(&self) -> Vec<TokenStream> {
        self.cfg_serde
            .iter()
            .map(|(predicate, items)| quote! { #[cfg_attr(#predicate, serde(#(#items),*))] })
            .collect()
    }
}

/// A `#[schema(..)]` attribute of a field `partial` wraps in `Option` (see
/// [`CarriedAttrs::for_option_wrap`]): `None` when nothing is left.
fn option_wrap_schema(attr: syn::Attribute) -> Option<syn::Attribute> {
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
}

/// Whether a `#[schema(..)]` attribute sets `default`.
fn schema_has_default(attr: &syn::Attribute) -> bool {
    schema_items(attr).is_some_and(|items| items.iter().any(|item| item.key == "default"))
}

/// Every `cfg(q)` inside a field's `#[cfg_attr(p, ..)]` (nested ones
/// included) hoisted to the equivalent top-level `#[cfg(any(not(p), q))]`:
/// the DTO field, its `From` assignment and its helpers read top-level
/// `#[cfg]`s only, so a source field the compiler drops by
/// `#[cfg_attr(p, cfg(q))]` used to be emitted (and read) unconditionally.
pub fn hoist_cfg_attr_cfgs(item: &mut syn::ItemStruct) {
    for field in &mut item.fields {
        let mut attrs = Vec::with_capacity(field.attrs.len());
        for attr in std::mem::take(&mut field.attrs) {
            let parsed = attr
                .path()
                .is_ident("cfg_attr")
                .then(|| attr.parse_args_with(parse_cfg_attr).ok())
                .flatten();
            let Some((predicate, metas)) = parsed else {
                attrs.push(attr);
                continue;
            };
            let mut hoisted = Vec::new();
            let kept = split_cfgs(&predicate, metas, &mut hoisted);
            if hoisted.is_empty() {
                attrs.push(attr);
                continue;
            }
            attrs.extend(hoisted);
            if !kept.is_empty() {
                attrs.push(syn::parse_quote! { #[cfg_attr(#predicate, #(#kept),*)] });
            }
        }
        field.attrs = attrs;
    }
}

/// The items of `cfg_attr(predicate, ..)` without their `cfg(q)`s, which
/// go to `hoisted` as `#[cfg(any(not(predicate), q))]` (a nested
/// `cfg_attr(inner, ..)` under `all(predicate, inner)`).
fn split_cfgs(
    predicate: &Predicate,
    metas: Punctuated<syn::Meta, syn::Token![,]>,
    hoisted: &mut Vec<syn::Attribute>,
) -> Vec<syn::Meta> {
    let mut kept = Vec::new();
    for meta in metas {
        let list = meta.require_list().ok();
        if meta.path().is_ident("cfg")
            && let Some(condition) = list.and_then(|list| list.parse_args::<Predicate>().ok())
        {
            hoisted.push(syn::parse_quote! { #[cfg(any(not(#predicate), #condition))] });
        } else if meta.path().is_ident("cfg_attr")
            && let Some((inner, metas)) =
                list.and_then(|list| list.parse_args_with(parse_cfg_attr).ok())
        {
            let combined: Predicate = syn::parse_quote!(all(#predicate, #inner));
            let inner_kept = split_cfgs(&combined, metas, hoisted);
            if !inner_kept.is_empty() {
                kept.push(syn::parse_quote! { cfg_attr(#inner, #(#inner_kept),*) });
            }
        } else {
            kept.push(meta);
        }
    }
    kept
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
        let cfg_schema = carried.cfg_schema_attrs();
        let other = &carried.other;
        let cfg_serde = carried.cfg_serde_attrs();
        quote! { #(#schema)* #(#cfg_schema)* #(#other)* #(#cfg_serde)* }.to_string()
    }

    #[rstest]
    // `serde` inside `cfg_attr` is carried (paths the source module knows
    // are re-rooted; `dflt` is unknown here), its `rename` too when the
    // field has none of its own; a non-list `serde` / unparseable list is
    // dropped.
    #[case::cfg_attr_serde(
        quote!(#[cfg_attr(feature = "x", serde(rename = "a", alias = "b", default = "dflt"), garde(skip))]),
        true,
        "# [cfg_attr (feature = \"x\" , garde (skip))] # [cfg_attr (feature = \"x\" , serde (rename = \"a\" , alias = \"b\" , default = \"dflt\"))]"
    )]
    // The cfg-gated rename names the field when the feature is on; it used
    // to be dropped, leaving the plain field name.
    #[case::cfg_attr_serde_only_rename(
        quote!(#[cfg_attr(all(), serde(rename = "a"))]),
        true,
        "# [cfg_attr (all () , serde (rename = \"a\"))]"
    )]
    // A field already renamed keeps one `rename`: the cfg-gated one goes.
    #[case::cfg_attr_serde_rename_already_renamed(
        quote!(#[serde(rename = "b")] #[cfg_attr(all(), serde(rename = "a", alias = "c"))]),
        true,
        "# [cfg_attr (all () , serde (alias = \"c\"))]"
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
    // Collected without a derive too: the metadata keeps the active ones.
    #[case::cfg_attr_schema_not_derived(
        quote!(#[cfg_attr(feature = "x", schema(minimum = 1))]),
        false,
        "# [cfg_attr (feature = \"x\" , schema (minimum = 1))]"
    )]
    #[case::cfg_attr_all_kinds(
        quote!(#[cfg_attr(all(), garde(skip), deprecated, doc = "d")]),
        true,
        "# [cfg_attr (all () , garde (skip) , deprecated , doc = \"d\")]"
    )]
    #[case::cfg_attr_unparseable(quote!(#[cfg_attr(feature = "x")]), true, "")]
    // `cfg_attr(true, ..)` (a literal predicate, Rust 1.88+) used to be dropped.
    #[case::cfg_attr_literal(
        quote!(#[cfg_attr(true, garde(skip), schema(minimum = 1))]),
        true,
        "# [cfg_attr (true , schema (minimum = 1))] # [cfg_attr (true , garde (skip))]"
    )]
    #[case::dropped(quote!(#[sea_orm(primary_key)] #[serde(default)]), true, "")]
    // L3 (round 14): a nested `cfg_attr` is flattened under `all(..)` (it
    // used to be dropped, so the DTO lost the source's cfg-gated rename).
    #[case::nested_cfg_attr(
        quote!(#[cfg_attr(feature = "pg", cfg_attr(not(test), serde(rename = "x"), garde(skip)), deprecated)]),
        true,
        "# [cfg_attr (all (feature = \"pg\" , not (test)) , garde (skip))] # [cfg_attr (feature = \"pg\" , deprecated)] # [cfg_attr (all (feature = \"pg\" , not (test)) , serde (rename = \"x\"))]"
    )]
    #[case::nested_cfg_attr_unparseable(
        quote!(#[cfg_attr(feature = "pg", cfg_attr(not(test)), cfg_attr)]),
        true,
        ""
    )]
    fn carries_expected_attributes(
        #[case] attrs: TokenStream,
        #[case] schema_derived: bool,
        #[case] expected: &str,
    ) {
        let module = ["crate", "m"].map(String::from);
        assert_eq!(
            render(&carried_field_attrs(
                &field(attrs),
                schema_derived,
                &module,
                false
            )),
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
    // A `schema(default ..)` inside `cfg_attr` is no unconditional
    // override: it only gates the SeaORM-derived default (it used to drop
    // it, so the generated serde default had no documented value with the
    // cfg off).
    #[case::cfg_attr_default(quote!(#[cfg_attr(feature = "x", schema(default = 5))]), false)]
    fn detects_schema_default(#[case] attrs: TokenStream, #[case] expected: bool) {
        assert_eq!(
            carried_field_attrs(&field(attrs), true, &[], false).schema_default,
            expected
        );
    }

    #[test]
    fn collects_cfg_default_predicates() {
        let attrs = field(quote!(
            #[cfg_attr(feature = "x", schema(default = 5))]
            #[cfg_attr(feature = "y", serde(default))]
            #[cfg_attr(feature = "z", serde(alias = "q"))]
        ));
        let carried = carried_field_attrs(&attrs, true, &[], false);
        let schema: Vec<String> = carried
            .schema_default_cfgs
            .iter()
            .map(|p| quote!(#p).to_string())
            .collect();
        let serde: Vec<String> = carried
            .cfg_serde_default_predicates()
            .iter()
            .map(|p| quote!(#p).to_string())
            .collect();
        assert_eq!(schema, ["feature = \"x\""]);
        assert_eq!(serde, ["feature = \"y\""]);
        assert!(carried.for_option_wrap().schema_default_cfgs.is_empty());
    }

    /// The `rename` parameter pins the wire name: a cfg-gated rename is
    /// dropped.
    #[test]
    fn renamed_field_drops_cfg_rename() {
        let attrs = field(quote!(#[cfg_attr(all(), serde(rename = "a"))]));
        assert_eq!(render(&carried_field_attrs(&attrs, true, &[], true)), "");
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
    // A `cfg_attr` schema gets the same rewrite.
    #[case::cfg_attr_schema(
        quote!(#[cfg_attr(feature = "x", schema(value_type = String, default = "5"))] #[cfg_attr(all(), schema(default = 1))]),
        "# [cfg_attr (feature = \"x\" , schema (value_type = :: std :: option :: Option < String >))]"
    )]
    fn option_wrap_attrs(#[case] attrs: TokenStream, #[case] expected: &str) {
        let carried = carried_field_attrs(&field(attrs), true, &[], false).for_option_wrap();
        assert!(!carried.schema_default);
        assert_eq!(render(&carried), expected);
    }

    #[rstest]
    #[case::cfg_and_garde(quote!(#[cfg(test)] #[cfg_attr(all(), garde(skip))]), true, false)]
    #[case::deprecated(quote!(#[deprecated]), false, true)]
    #[case::cfg_attr_deprecated(quote!(#[cfg_attr(all(), deprecated)]), false, true)]
    fn flags(#[case] attrs: TokenStream, #[case] garde: bool, #[case] deprecated: bool) {
        let carried = carried_field_attrs(&field(attrs), true, &[], false);
        assert_eq!((carried.garde, carried.deprecated), (garde, deprecated));
    }

    /// L11 (round 13): the `cfg_attr` schema items whose predicate holds
    /// go to the metadata of an `ignore` type; unknown / false ones do not.
    #[test]
    fn active_cfg_schema_keeps_known_true_predicates() {
        let attrs = field(quote!(
            #[cfg_attr(all(), schema(minimum = 1))]
            #[cfg_attr(any(), schema(maximum = 2))]
            #[cfg_attr(unix_like_unknown, schema(maximum = 3))]
        ));
        let carried = carried_field_attrs(&attrs, false, &[], false);
        let active = carried.active_cfg_schema();
        assert_eq!(quote!(#(#active)*).to_string(), "# [schema (minimum = 1)]");
        assert!(carried.schema_default_cfgs.is_empty());
    }

    #[test]
    fn collects_cfgs() {
        let carried = carried_field_attrs(&field(quote!(#[cfg(test)])), true, &[], false);
        assert_eq!(carried.cfgs.len(), 1);
    }

    /// L1 (round 15): a `cfg(q)` inside `cfg_attr(p, ..)` becomes the
    /// top-level `cfg(any(not(p), q))` (it was dropped: the DTO always had
    /// the field, its `From` read a field the source did not have).
    #[rstest]
    #[case::alone(
        quote!(#[cfg_attr(feature = "x", cfg(test))]),
        "# [cfg (any (not (feature = \"x\") , test))]"
    )]
    #[case::with_other_items(
        quote!(#[cfg_attr(feature = "x", cfg(test), serde(default))]),
        "# [cfg (any (not (feature = \"x\") , test))] # [cfg_attr (feature = \"x\" , serde (default))]"
    )]
    #[case::nested(
        quote!(#[cfg_attr(a, cfg_attr(b, cfg(c), doc = "d"), deprecated)]),
        "# [cfg (any (not (all (a , b)) , c))] # [cfg_attr (a , cfg_attr (b , doc = \"d\") , deprecated)]"
    )]
    #[case::nested_cfg_only(
        quote!(#[cfg_attr(a, cfg_attr(b, cfg(c)))]),
        "# [cfg (any (not (all (a , b)) , c))]"
    )]
    #[case::bool_predicate(
        quote!(#[cfg_attr(true, cfg(false))]),
        "# [cfg (any (not (true) , false))]"
    )]
    #[case::no_cfg_kept(
        quote!(#[cfg_attr(a, cfg_attr(b, doc = "d"))] #[serde(default)]),
        "# [cfg_attr (a , cfg_attr (b , doc = \"d\"))] # [serde (default)]"
    )]
    #[case::unparseable_cfg_kept(
        quote!(#[cfg_attr(a, cfg = "x", cfg(1 + 2))] #[cfg_attr]),
        "# [cfg_attr (a , cfg = \"x\" , cfg (1 + 2))] # [cfg_attr]"
    )]
    #[case::unparseable_nested_kept(
        quote!(#[cfg_attr(a, cfg_attr(b), cfg(c))]),
        "# [cfg (any (not (a) , c))] # [cfg_attr (a , cfg_attr (b))]"
    )]
    fn hoists_cfg_inside_cfg_attr(#[case] attrs: TokenStream, #[case] expected: &str) {
        let mut item: syn::ItemStruct = syn::parse_quote! { struct S { #attrs pub value: i32 } };
        hoist_cfg_attr_cfgs(&mut item);
        let attrs = &item.fields.iter().next().unwrap().attrs;
        assert_eq!(quote!(#(#attrs)*).to_string(), expected);
    }
}
