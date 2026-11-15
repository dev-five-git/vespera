//! The parameter checks that need the emitted fields: a `rename` /
//! `partial` / `pick` / `relation_adapters` entry naming a field the type
//! does not emit (or cannot apply to) is an error, not a silent no-op.

use std::collections::{HashMap, HashSet};

use super::super::from_model::FieldMapping;
use super::super::input::SchemaTypeInput;
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
}

pub(super) fn check_emitted(
    input: &SchemaTypeInput,
    partial_fields: Option<&Vec<String>>,
    emitted: &Emitted<'_>,
    at: &SpannedSource<'_>,
    source_type_name: &str,
) -> syn::Result<()> {
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
        (
            "pick",
            input.pick.iter().flatten().map(String::as_str).collect(),
        ),
    ] {
        validate_emitted(param, names, emitted.dropped, at)?;
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
            )
        }),
        input.rename.as_ref(),
        input.add.as_ref(),
        at,
    )
}
