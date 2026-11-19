//! Input parsing for schema macros
//!
//! Defines input structures for `schema!` and `schema_type!` macros.

use syn::{
    Ident, LitStr, Token, Type, bracketed, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

/// Input for the schema! macro
///
/// Supports:
/// - `schema!(Type)` - Full schema
/// - `schema!(Type, omit = ["field1", "field2"])` - Schema with fields omitted
/// - `schema!(Type, pick = ["field1", "field2"])` - Schema with only specified fields
pub struct SchemaInput {
    /// The type to generate schema for
    pub ty: Type,
    /// Fields to omit from the schema
    pub omit: Option<Vec<String>>,
    /// Fields to pick (include only these fields)
    pub pick: Option<Vec<String>>,
    /// Where each `omit` / `pick` name was written.
    pub spans: ParamSpans,
}

impl Parse for SchemaInput {
    #[allow(clippy::too_many_lines)]
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse the type
        let ty: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;
        let mut spans = ParamSpans::default();

        // Parse optional parameters
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "omit" => {
                    // Reject a second `omit` instead of silently overwriting the
                    // first (the prior behaviour gave surprising schemas with no
                    // diagnostic) — matching `schema_type!`'s stricter parser.
                    if omit.is_some() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "duplicate parameter `omit` in schema! invocation",
                        ));
                    }
                    if pick.is_some() {
                        return Err(both_filters_error(ident.span()));
                    }
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> = content
                        .parse_terminated(syn::parse::ParseBuffer::parse::<LitStr>, Token![,])?;
                    for lit in &fields {
                        spans.record("omit", lit);
                    }
                    omit = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "pick" => {
                    if pick.is_some() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "duplicate parameter `pick` in schema! invocation",
                        ));
                    }
                    if omit.is_some() {
                        return Err(both_filters_error(ident.span()));
                    }
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> = content
                        .parse_terminated(syn::parse::ParseBuffer::parse::<LitStr>, Token![,])?;
                    if fields.is_empty() {
                        return Err(empty_pick_error(ident.span()));
                    }
                    for lit in &fields {
                        spans.record("pick", lit);
                    }
                    pick = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown parameter: `{ident_str}`. Expected `omit` or `pick`"),
                    ));
                }
            }
        }

        Ok(Self {
            ty,
            omit,
            pick,
            spans,
        })
    }
}

/// `schema!` given both filters: reported at the second one.
fn both_filters_error(span: proc_macro2::Span) -> syn::Error {
    syn::Error::new(
        span,
        "schema! macro: cannot use both `omit` and `pick` in the same invocation. Use one or the other to filter fields.",
    )
}

/// Input for the `schema_type`! macro
///
/// Syntax: `schema_type!(NewTypeName from SourceType, pick = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, omit = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, rename = [("old", "new")])`
/// Or:     `schema_type!(NewTypeName from SourceType, add = [("field": Type)])`
/// Or:     `schema_type!(NewTypeName from SourceType, ignore)` - skip Schema derive
/// Or:     `schema_type!(NewTypeName from SourceType, name = "CustomName")` - custom `OpenAPI` name
/// Or:     `schema_type!(NewTypeName from SourceType, rename_all = "camelCase")` - serde `rename_all`
#[allow(clippy::struct_excessive_bools)]
pub struct SchemaTypeInput {
    /// The new type name to generate
    pub new_type: Ident,
    /// The source type to derive from
    pub source_type: Type,
    /// Fields to omit from the new type
    pub omit: Option<Vec<String>>,
    /// Fields to pick (include only these fields)
    pub pick: Option<Vec<String>>,
    /// Field renames: (`source_field_name`, `new_field_name`)
    pub rename: Option<Vec<(String, String)>>,
    /// Explicit same-file adapter DTOs for single-value relation fields:
    /// (`relation_field_name`, `AdapterStructIdent`)
    pub relation_adapters: Vec<(String, Ident)>,
    /// New fields to add: (`field_name`, `field_type`)
    pub add: Option<Vec<(String, Type)>>,
    /// Whether to derive Clone (default: true)
    pub derive_clone: bool,
    /// Fields to wrap in `Option<T>` for partial updates.
    ///
    /// - `partial` (bare) = all fields become `Option<T>`
    /// - `partial = ["field1", "field2"]` = only listed fields become `Option<T>`
    /// - Fields already `Option<T>` are left unchanged.
    pub partial: Option<PartialMode>,
    /// Whether to skip deriving the Schema trait (default: false)
    /// Use `ignore` keyword to set this to true.
    pub ignore_schema: bool,
    /// Custom `OpenAPI` schema name (overrides Rust struct name)
    /// Use `name = "CustomName"` to set this.
    pub schema_name: Option<String>,
    /// Serde `rename_all` strategy (e.g., "camelCase", "`snake_case`", "`PascalCase`")
    /// If not specified, defaults to "camelCase" when source has no `rename_all`
    pub rename_all: Option<String>,
    /// Whether to generate a multipart/form-data struct (derives `Multipart` instead of serde)
    /// Use `multipart` bare keyword to set this to true.
    pub multipart: bool,
    /// Whether to omit fields that have database defaults (sea_orm `default_value` or `primary_key`).
    /// Use `omit_default` bare keyword to set this to true.
    pub omit_default: bool,
    /// Where each field name of the field-list parameters was written, so
    /// a diagnostic about one points at its literal.
    pub spans: ParamSpans,
}

/// The source spans of the field names `schema_type!` was given, keyed by
/// parameter (`omit`, `pick`, `partial`, `rename`, `rename_to`, `add`,
/// `relation_adapters`) and name.
#[derive(Clone, Debug, Default)]
pub struct ParamSpans {
    fields: Vec<(&'static str, String, proc_macro2::Span)>,
}

impl ParamSpans {
    fn record(&mut self, param: &'static str, lit: &LitStr) {
        self.fields.push((param, lit.value(), lit.span()));
    }

    /// The span of `name` as written in `param` (`None` when it was not).
    pub fn field(&self, param: &str, name: &str) -> Option<proc_macro2::Span> {
        self.occurrence(param, name, 0)
    }

    /// The span of the `nth` (0-based) time `name` is written in `param`:
    /// a repeated entry's diagnostic points at the repetition, not at the
    /// first one.
    pub fn occurrence(&self, param: &str, name: &str, nth: usize) -> Option<proc_macro2::Span> {
        self.fields
            .iter()
            .filter(|(p, n, _)| *p == param && n == name)
            .nth(nth)
            .map(|(_, _, span)| *span)
    }
}

/// Mode for the `partial` keyword in `schema_type`!
#[derive(Clone, Debug)]
pub enum PartialMode {
    /// All fields become Option<T>
    All,
    /// Only listed fields become Option<T>
    Fields(Vec<String>),
}

/// Helper struct to parse an add field: ("`field_name"`: Type)
struct AddField {
    name: LitStr,
    ty: Type,
}

impl Parse for AddField {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let name: LitStr = content.parse()?;
        content.parse::<Token![:]>()?;
        let ty: Type = content.parse()?;
        Ok(Self { name, ty })
    }
}

/// Helper struct to parse a rename pair: ("`old_name`", "`new_name`")
struct RenamePair {
    from: LitStr,
    to: LitStr,
}

impl Parse for RenamePair {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let from: LitStr = content.parse()?;
        content.parse::<Token![,]>()?;
        let to: LitStr = content.parse()?;
        Ok(Self { from, to })
    }
}

/// Helper struct to parse a relation adapter pair: ("`field_name`", AdapterIdent)
struct RelationAdapterPair {
    field_name: LitStr,
    adapter: Ident,
}

impl Parse for RelationAdapterPair {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let field_name: LitStr = content.parse()?;
        content.parse::<Token![,]>()?;
        let adapter: Ident = content.parse()?;
        Ok(Self {
            field_name,
            adapter,
        })
    }
}

impl Parse for SchemaTypeInput {
    #[allow(clippy::too_many_lines)]
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse new type name
        let new_type: Ident = input.parse()?;

        // Parse "from" keyword
        let from_ident: Ident = input.parse()?;
        if from_ident != "from" {
            return Err(syn::Error::new(
                from_ident.span(),
                format!(
                    "schema_type! macro: expected `from` keyword, found `{from_ident}`. Use format: `schema_type!(NewType from SourceType, ...)`."
                ),
            ));
        }

        // Parse source type
        let source_type: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;
        let mut rename = None;
        let mut relation_adapters = Vec::new();
        let mut add = None;
        let mut derive_clone = true;
        let mut partial = None;
        let mut ignore_schema = false;
        let mut schema_name = None;
        let mut rename_all = None;
        let mut multipart = false;
        let mut omit_default = false;
        let mut spans = ParamSpans::default();
        // Reject a repeated parameter (e.g. `pick = .., pick = ..` or a bare
        // `partial, partial`) with a spanned error instead of letting the
        // later value silently overwrite the earlier one.
        let mut seen_params = std::collections::HashSet::<String>::new();

        // Parse optional parameters
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: Ident = input.parse()?;
            let ident_str = ident.to_string();
            if !seen_params.insert(ident_str.clone()) {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("duplicate parameter `{ident_str}` in schema_type! macro"),
                ));
            }

            match ident_str.as_str() {
                "omit" | "pick" => {
                    // The second of the two is the conflicting one.
                    if omit.is_some() || pick.is_some() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "schema_type! macro: cannot use both `omit` and `pick` in the same invocation. Use one or the other to filter fields.",
                        ));
                    }
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> = content
                        .parse_terminated(syn::parse::ParseBuffer::parse::<LitStr>, Token![,])?;
                    let param = if ident_str == "omit" { "omit" } else { "pick" };
                    if param == "pick" && fields.is_empty() {
                        return Err(empty_pick_error(ident.span()));
                    }
                    for lit in &fields {
                        spans.record(param, lit);
                    }
                    let names = Some(fields.into_iter().map(|s| s.value()).collect());
                    if param == "omit" {
                        omit = names;
                    } else {
                        pick = names;
                    }
                }
                "rename" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let pairs: Punctuated<RenamePair, Token![,]> =
                        content.parse_terminated(RenamePair::parse, Token![,])?;
                    for pair in &pairs {
                        spans.record("rename", &pair.from);
                        spans.record("rename_to", &pair.to);
                    }
                    rename = Some(
                        pairs
                            .into_iter()
                            .map(|p| (p.from.value(), p.to.value()))
                            .collect(),
                    );
                }
                "relation_adapters" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let pairs: Punctuated<RelationAdapterPair, Token![,]> =
                        content.parse_terminated(RelationAdapterPair::parse, Token![,])?;
                    for pair in &pairs {
                        spans.record("relation_adapters", &pair.field_name);
                    }
                    relation_adapters = pairs
                        .into_iter()
                        .map(|p| (p.field_name.value(), p.adapter))
                        .collect();
                }
                "add" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<AddField, Token![,]> =
                        content.parse_terminated(AddField::parse, Token![,])?;
                    if fields.is_empty() {
                        return Err(empty_add_error(ident.span()));
                    }
                    for field in &fields {
                        spans.record("add", &field.name);
                    }
                    add = Some(fields.into_iter().map(|f| (f.name.value(), f.ty)).collect());
                }
                "clone" => {
                    input.parse::<Token![=]>()?;
                    let value: syn::LitBool = input.parse()?;
                    derive_clone = value.value();
                }
                "partial" => {
                    if input.peek(Token![=]) {
                        // partial = ["field1", "field2"]
                        input.parse::<Token![=]>()?;
                        let content;
                        let _ = bracketed!(content in input);
                        let fields: Punctuated<LitStr, Token![,]> = content.parse_terminated(
                            syn::parse::ParseBuffer::parse::<LitStr>,
                            Token![,],
                        )?;
                        for lit in &fields {
                            spans.record("partial", lit);
                        }
                        partial = Some(PartialMode::Fields(
                            fields.into_iter().map(|s| s.value()).collect(),
                        ));
                    } else {
                        // bare `partial` - all fields
                        partial = Some(PartialMode::All);
                    }
                }
                "ignore" => {
                    // bare `ignore` - skip Schema derive
                    ignore_schema = true;
                }
                "name" => {
                    // name = "CustomSchemaName" - custom OpenAPI schema name
                    input.parse::<Token![=]>()?;
                    let name_lit: LitStr = input.parse()?;
                    schema_name = Some(name_lit.value());
                }
                "rename_all" => {
                    // rename_all = "camelCase" - serde rename_all strategy,
                    // checked against serde's rules (see `rename_rule`).
                    input.parse::<Token![=]>()?;
                    let rename_all_lit: LitStr = input.parse()?;
                    rename_all = Some(super::transformation::rename_rule(&rename_all_lit)?);
                }
                "multipart" => {
                    // bare `multipart` - derive Multipart instead of serde
                    multipart = true;
                }
                "omit_default" => {
                    // bare `omit_default` - omit fields with database defaults
                    omit_default = true;
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown parameter: `{ident_str}`. Expected `omit`, `pick`, `rename`, `relation_adapters`, `add`, `clone`, `partial`, `ignore`, `name`, `rename_all`, `multipart`, or `omit_default`"
                        ),
                    ));
                }
            }
        }

        let parsed = Self {
            new_type,
            source_type,
            omit,
            pick,
            rename,
            relation_adapters,
            add,
            derive_clone,
            partial,
            ignore_schema,
            schema_name,
            rename_all,
            multipart,
            omit_default,
            spans,
        };
        Ok(parsed)
    }
}

/// `pick = []`: an allowlist of no field. Every filter reads an empty pick
/// set as "no pick given", so it would silently keep every field.
fn empty_pick_error(span: proc_macro2::Span) -> syn::Error {
    syn::Error::new(
        span,
        "`pick = []` selects no field: list at least one field name (an empty `pick` would otherwise keep every field)",
    )
}

/// `add = []`: no field added. An empty `add` is not "no add": it still
/// disables the `From` / `from_model` generation the `add` gate guards.
fn empty_add_error(span: proc_macro2::Span) -> syn::Error {
    syn::Error::new(
        span,
        "`add = []` adds no field: omit the parameter (an empty `add` would otherwise disable the `From` impl)",
    )
}
#[cfg(test)]
mod tests;
