//! The parameter checks that need the emitted fields: a `rename` /
//! `partial` / `pick` / `relation_adapters` entry naming a field the type
//! does not emit (or cannot apply to) is an error, not a silent no-op.

use std::collections::{HashMap, HashSet};

use syn::punctuated::Punctuated;

use crate::parser::strip_raw_prefix_owned;

use super::super::from_model::FieldMapping;
use super::super::input::SchemaTypeInput;
use super::super::transformation::unreadable_serde_path;
use super::super::validation::{
    ErrorAt, SpannedSource, validate_emitted, validate_unique_field_names,
};

/// What the field loop recorded for the checks.
pub(super) struct Emitted<'a> {
    /// Source fields `omit` / `pick` keep that the type still does not
    /// emit, with the reason.
    pub dropped: &'a HashMap<String, &'static str>,
    /// The included SeaORM relation fields.
    pub relation_names: &'a HashSet<String>,
    /// The `relation_adapters` entries a HasOne / BelongsTo field used.
    pub used_adapters: &'a HashSet<String>,
    pub field_mappings: &'a [FieldMapping],
    /// The Rust identifiers `pick` selects (each name resolved, see
    /// `field_selection`).
    pub picked: &'a HashSet<String>,
}

/// The source names of the emitted fields (`#[cfg]` twins share one).
pub(super) fn emitted_source_names(field_mappings: &[FieldMapping]) -> HashSet<String> {
    field_mappings
        .iter()
        .map(|mapping| strip_raw_prefix_owned(mapping.source_ident.to_string()))
        .collect()
}

/// The dropped names that no emitted field carries. A `#[cfg]` twin dropped by
/// `omit_default` leaves its sibling twin emitted, so the name is not dropped
/// (R28-SM2-L1: `rename` / `pick` / `partial` used to reject it).
fn names_not_emitted(emitted: &Emitted<'_>) -> HashMap<String, &'static str> {
    let emitted_names = emitted_source_names(emitted.field_mappings);
    emitted
        .dropped
        .iter()
        .filter(|(name, _)| !emitted_names.contains(name.as_str()))
        .map(|(name, reason)| (name.clone(), *reason))
        .collect()
}

pub(super) fn check_emitted(
    input: &SchemaTypeInput,
    partial_fields: Option<&Vec<String>>,
    emitted: &Emitted<'_>,
    at: &SpannedSource<'_>,
    source_type_name: &str,
) -> syn::Result<()> {
    let dropped = names_not_emitted(emitted);
    let mut picked: Vec<&str> = emitted.picked.iter().map(String::as_str).collect();
    picked.sort_unstable();
    for (param, names) in [
        (
            "rename",
            input
                .rename
                .iter()
                .flatten()
                .map(|(from, _)| from.as_str())
                .collect::<Vec<_>>(),
        ),
        (
            "partial",
            partial_fields
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect(),
        ),
        ("pick", picked),
    ] {
        validate_emitted(param, names, &dropped, at)?;
    }
    // `partial` never wraps an (included) relation, whose shape comes from the
    // relation conversion; naming one used to be silently ignored.
    if let Some(field) = partial_fields
        .into_iter()
        .flatten()
        .find(|field| emitted.relation_names.contains(*field))
    {
        return Err(at.error_at("partial", field, format!("`partial` names the SeaORM relation `{field}`, which `partial` never makes optional: omit it (`omit = [\"{field}\"]`) or declare the patch field with `add`")));
    }
    // An adapter only replaces an included HasOne / BelongsTo relation: one
    // for a plain field, a HasMany, or in `multipart` mode used to be
    // silently ignored.
    if let Some((field, _)) = input
        .relation_adapters
        .iter()
        .find(|(field, _)| !emitted.used_adapters.contains(field))
    {
        return Err(at.error_at(
            "relation_adapters",
            field,
            format!(
                "`relation_adapters`: field `{field}` of `{source_type_name}` is not a HasOne / BelongsTo SeaORM relation this type includes{}",
                if input.multipart { " (`multipart` types carry no relations)" } else { "" }
            ),
        ));
    }
    validate_unique_field_names(
        emitted.field_mappings.iter().map(|mapping| {
            (
                mapping.new_ident.to_string(),
                mapping.source_ident.to_string(),
                mapping.cfg_undecided,
            )
        }),
        input.rename.as_ref(),
        input.add.as_ref(),
        at,
    )
}

/// The source container's serde function path (`#[serde(default = "f")]`)
/// must name an item the call site can read when the DTO carries it: the
/// default helper calls `f` from the DTO's module (R41-B2-01). `carried` is
/// whether the DTO is built from the source (the helper is emitted only then).
pub(super) fn check_container_default_path(
    attrs: &[syn::Attribute],
    carried: bool,
    source_module: &[String],
) -> syn::Result<()> {
    if !carried {
        return Ok(());
    }
    let call_site = super::super::file_lookup::call_site_module();
    unreadable_serde_path(attrs, source_module, call_site.as_deref()).map_or(Ok(()), Err)
}

/// The serde function paths of the emitted fields must name items the call
/// site can read (a private `default_name` of the source module is `E0603` in
/// the DTO emitted elsewhere).
pub(super) fn check_serde_paths(
    fields: &syn::Fields,
    emitted: &HashSet<String>,
    source_module: &[String],
) -> syn::Result<()> {
    let call_site = super::super::file_lookup::call_site_module();
    for field in fields {
        let emitted_field = field
            .ident
            .as_ref()
            .is_some_and(|ident| emitted.contains(&strip_raw_prefix_owned(ident.to_string())));
        if emitted_field
            && let Some(error) =
                unreadable_serde_path(&field.attrs, source_module, call_site.as_deref())
        {
            return Err(error);
        }
    }
    Ok(())
}

/// `#[serde(transparent)]` (copied from the source) is serde's newtype form:
/// it needs exactly one field in the DTO, which `add` / `omit` can change
/// (SM4-02, round 40). `emitted` counts the source fields the DTO keeps.
pub(super) fn check_transparent(
    attrs: &[syn::Attribute],
    input: &SchemaTypeInput,
    emitted: usize,
) -> syn::Result<()> {
    let transparent = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .any(|attr| {
            attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .is_ok_and(|items| items.iter().any(|meta| meta.path().is_ident("transparent")))
        });
    // A multipart DTO carries no `serde(transparent)` (R41-B2-04): nothing to keep.
    let field_count = emitted + input.add.as_ref().map_or(0, Vec::len);
    if transparent && !input.multipart && field_count != 1 {
        return Err(syn::Error::new(
            input.new_type.span(),
            format!(
                "schema_type!: `{}` would keep the source's `#[serde(transparent)]`, which needs exactly one field, but emits {field_count}: drop `transparent` from the source or keep one field",
                input.new_type
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::one_field_keeps_it(r"#[serde(transparent)]", "Dto from Src", 1, true)]
    #[case::two_fields_refused(r"#[serde(transparent)]", "Dto from Src", 2, false)]
    #[case::no_field_refused(r"#[serde(transparent)]", "Dto from Src", 0, false)]
    #[case::added_field_counts(
        r"#[serde(transparent)]",
        "Dto from Src, add = [(\"extra\": i32)]",
        1,
        false
    )]
    #[case::beside_other_keys(
        r"#[serde(deny_unknown_fields, transparent)]",
        "Dto from Src",
        3,
        false
    )]
    #[case::not_transparent(r"#[serde(deny_unknown_fields)]", "Dto from Src", 3, true)]
    #[case::not_serde(r"#[transparent]", "Dto from Src", 3, true)]
    // R41-B2-04: a multipart DTO never keeps the attribute, whatever its count.
    #[case::multipart_carries_no_transparent(
        r"#[serde(transparent)]",
        "Dto from Src, multipart",
        2,
        true
    )]
    fn transparent_needs_exactly_one_field(
        #[case] attr: &str,
        #[case] dsl: &str,
        #[case] emitted: usize,
        #[case] accepted: bool,
    ) {
        let source: syn::DeriveInput = syn::parse_str(&format!("{attr} struct S;")).unwrap();
        let input: SchemaTypeInput = syn::parse_str(dsl).unwrap();
        assert_eq!(
            check_transparent(&source.attrs, &input, emitted).is_ok(),
            accepted
        );
    }
}
