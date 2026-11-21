//! Field validation logic for `schema_type`! macro.
//!
//! This module contains functions to validate that fields specified in
//! pick, omit, rename, and partial parameters exist in the source struct.
//!
//! # Overview
//!
//! The `schema_type`! macro accepts user-specified field filters (pick, omit, rename, partial).
//! This module validates that all specified fields actually exist in the source struct,
//! providing clear error messages when fields don't exist.
//!
//! # Validation Functions
//!
//! - [`resolve_filter_names`] - Resolve every pick / omit name to a field (Rust
//!   identifier, then serde JSON name; see `field_selection`), refusing an
//!   unknown, skipped or ambiguous one
//! - [`validate_rename_fields`] - Ensure all rename source fields exist and
//!   every rename target is a legal Rust identifier
//! - [`validate_partial_fields`] - Ensure all partial fields exist
//! - [`validate_relation_adapters`] - Ensure every adapter names a distinct
//!   source field
//! - [`validate_add_field_idents`] - Ensure every `add` field name is a legal
//!   Rust identifier
//! - [`validate_unique_field_names`] - Refuse a rename target or `add` field
//!   that reuses another field's name in the generated type
//! - [`validate_included`] - Ensure `rename` / `partial` /
//!   `relation_adapters` name fields the DTO keeps
//! - [`validate_emitted`] - Ensure `rename` / `partial` /
//!   `relation_adapters` name fields the DTO emits
//! - [`extract_source_field_names`] - Extract all field names from a struct
//!
//! # Example
//!
//! ```ignore
//! // This validates that "user_id", "name" exist in Model
//! schema_type!(UserResponse from Model, pick = ["user_id", "name"]);
//!
//! // If "nonexistent" doesn't exist, validation error is raised at compile time
//! schema_type!(BadSchema from Model, pick = ["nonexistent"]);
//! ```

use std::collections::{BTreeSet, HashSet};

use super::field_selection::{Refusal, SourceField, resolve_name, source_fields};
use super::input::ParamSpans;

/// Where a diagnostic about a field name given to a parameter points.
pub trait ErrorAt {
    /// The error for `field` as given to `param`.
    fn error_at(&self, param: &str, field: &str, message: String) -> syn::Error {
        self.error_at_occurrence(param, field, 0, message)
    }

    /// The error for the `nth` (0-based) time `field` is given to `param`
    /// (a repeated entry: the repetition).
    fn error_at_occurrence(
        &self,
        param: &str,
        field: &str,
        nth: usize,
        message: String,
    ) -> syn::Error;
}

/// Without recorded spans: the source type.
impl ErrorAt for syn::Type {
    fn error_at_occurrence(&self, _: &str, _: &str, _: usize, message: String) -> syn::Error {
        syn::Error::new_spanned(self, message)
    }
}

/// The literal the name was written as, else the source type.
pub struct SpannedSource<'a> {
    pub source_type: &'a syn::Type,
    pub spans: &'a ParamSpans,
}

impl ErrorAt for SpannedSource<'_> {
    fn error_at_occurrence(
        &self,
        param: &str,
        field: &str,
        nth: usize,
        message: String,
    ) -> syn::Error {
        match self.spans.occurrence(param, field, nth) {
            Some(span) => syn::Error::new(span, message),
            None => syn::Error::new_spanned(self.source_type, message),
        }
    }
}

fn sorted_source_fields(source_field_names: &HashSet<String>) -> Vec<&str> {
    source_field_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Checks that each `rename` / `partial` / `relation_adapters` name is a
/// source field name ([`extract_source_field_names`]). A name of a field a
/// `#[cfg]` (or `feature`) removes in this build is accepted on purpose: the
/// DTO may rename or patch a feature-gated field, and the entry only takes
/// effect in the builds that keep the field (R28-SM4-L2, decided). `pick` /
/// `omit` refuse such a name instead.
fn validate_fields_exist<'a>(
    kind: &str,
    fields: impl IntoIterator<Item = &'a str>,
    source_field_names: &HashSet<String>,
    at: &impl ErrorAt,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    for field in fields {
        if !source_field_names.contains(field) {
            let prefix = match kind {
                "partial" => "partial field",
                "relation_adapters" => "relation adapter field",
                _ => "field",
            };
            return Err(at.error_at(
                kind,
                field,
                format!(
                    "{prefix} `{field}` does not exist in type `{source_type_name}`. Available fields: {:?}",
                    sorted_source_fields(source_field_names)
                ),
            ));
        }
    }
    Ok(())
}

/// The Rust identifiers the `param` (`pick` / `omit`) names select in
/// `source`, each name read as `schema!` reads it ([`resolve_name`]: Rust
/// identifier first, then serde JSON name). A name that selects nothing, names
/// a serde-skipped field, or is an ambiguous JSON name is a spanned error.
pub fn resolve_filter_names(
    param: &str,
    names: Option<&Vec<String>>,
    source: &syn::ItemStruct,
    source_type_name: &str,
    at: &impl ErrorAt,
) -> Result<HashSet<String>, syn::Error> {
    let mut selected = HashSet::new();
    for name in names.into_iter().flatten() {
        let rust = match resolve_name(source, name) {
            Ok(rust) => rust,
            Err(Refusal::Missing {
                skipped: Some(rust),
            }) => {
                return Err(at.error_at(
                    param,
                    name,
                    format!(
                        "`{param}` names field `{name}`, which this type does not include: serde does not serialize it (`{rust}` is `#[serde(skip)]` or `skip_serializing`)"
                    ),
                ));
            }
            Err(Refusal::Missing { skipped: None }) => {
                return Err(at.error_at(
                    param,
                    name,
                    format!(
                        "field `{name}` does not exist in type `{source_type_name}`. Available fields: {:?}",
                        sorted_source_fields(&listed_source_field_names(source))
                    ),
                ));
            }
            Err(Refusal::Ambiguous(owners)) => {
                return Err(at.error_at(
                    param,
                    name,
                    format!(
                        "`{name}` is the JSON name of more than one field of `{source_type_name}` ({owners:?}); name the field by its Rust identifier"
                    ),
                ));
            }
        };
        selected.insert(rust);
    }
    Ok(selected)
}

/// Returns `true` when `name` is a legal Rust identifier, i.e. the field
/// identifier `module_scope::segment_ident` emits for a `rename` / `add`
/// target names the same field as `name`, so the collision and keyword guards
/// check the text that is emitted. A name reserved only in a later edition
/// (`gen`) is emitted as its raw identifier (`r#gen`): the same field, not the
/// same spelling (R36-SM5-L1).
///
/// `syn::parse_str::<Ident>` rejects non-identifiers (`"user-id"`,
/// `"a b"`, `""`, a leading digit) AND reserved keywords (`"type"`,
/// `"match"`). It also tokenizes its input, so padded or commented text
/// (`" id "`, `"id /* c */"`) parses to a trimmed identifier: the round-trip
/// check refuses those, since they would pass the guards as written and be
/// emitted as `id`. Raw identifiers (`"r#type"`) are accepted, but not the
/// path keywords with a raw prefix (`"r#self"`, `"r#super"`, `"r#crate"`,
/// `"r#Self"`, `"r#_"`): syn accepts those, and the emitted field would lose
/// its `r#` (R20-S4-L2, round 20).
fn is_valid_field_ident(name: &str) -> bool {
    syn::parse_str::<syn::Ident>(name).is_ok_and(|ident| ident == name)
        && !matches!(
            name.strip_prefix("r#").unwrap_or(name),
            "self" | "Self" | "super" | "crate" | "_"
        )
}

/// Validates a `rename` pair list: every **source** field must exist in
/// the source struct, and every **target** name must be a legal Rust
/// identifier.
///
/// The target check is what stops a `schema_type!(.., rename = [("id",
/// "user-id")])` (or a keyword target like `"type"`) from reaching
/// `module_scope::segment_ident` as a field name it cannot emit; it surfaces
/// as a spanned compile error instead of an opaque expansion abort.
pub fn validate_rename_fields(
    rename_pairs: Option<&Vec<(String, String)>>,
    source_field_names: &HashSet<String>,
    at: &impl ErrorAt,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    validate_fields_exist(
        "rename",
        rename_pairs
            .into_iter()
            .flatten()
            .map(|(from_field, _)| from_field.as_str()),
        source_field_names,
        at,
        source_type_name,
    )?;
    let mut seen = HashSet::new();
    for (from_field, to_field) in rename_pairs.into_iter().flatten() {
        // A second rename of one field used to win silently.
        if !seen.insert(from_field.as_str()) {
            return Err(at.error_at_occurrence(
                "rename",
                from_field,
                1,
                format!("`rename` lists source field `{from_field}` more than once"),
            ));
        }
        if !is_valid_field_ident(to_field) {
            return Err(at.error_at(
                "rename_to",
                to_field,
                format!(
                    "rename target `{to_field}` (for source field `{from_field}`) is not a valid \
                     Rust identifier; use letters/digits/`_` (not starting with a digit) and avoid \
                     reserved keywords"
                ),
            ));
        }
    }
    Ok(())
}

/// Validates that every `add = [(name: Type)]` field name is a legal Rust
/// identifier (see [`is_valid_field_ident`]): the field
/// `module_scope::segment_ident` materializes for it is the same field as
/// `name` (an edition-reserved name is emitted raw, `r#gen`), so a
/// non-identifier / keyword name is refused here like an invalid `rename`
/// target.
pub fn validate_add_field_idents(
    add: Option<&Vec<(String, syn::Type)>>,
    at: &impl ErrorAt,
) -> Result<(), syn::Error> {
    for (name, _) in add.into_iter().flatten() {
        if !is_valid_field_ident(name) {
            return Err(at.error_at(
                "add",
                name,
                format!(
                    "`add` field name `{name}` is not a valid Rust identifier; use \
                     letters/digits/`_` (not starting with a digit) and avoid reserved keywords"
                ),
            ));
        }
    }
    Ok(())
}

/// Validates that no `rename` target or `add` field reuses the name of
/// another field of the generated type (`kept` are the emitted source
/// fields' `(emitted, source)` names): rustc would only report E0124 at a
/// generated ident, without naming the argument at fault.  Two kept source
/// fields of one source name (`#[cfg]` twins, renamed or not) are not this
/// argument's doing and stay allowed.
///
/// `kept` entries are `(emitted, source, undecided)`: `undecided` marks a field
/// whose `#[cfg]` is neither known true nor known false. The compiler may drop
/// it, so neither a rename target nor an `add` field equal to its name is
/// refused here (R34-SM5-2, R35-VAL-1): a real duplicate in the build that
/// keeps both is rustc's E0124 on the emitted field, and refusing it on every
/// platform would reject a rename the build without the field accepts.
pub fn validate_unique_field_names(
    kept: impl IntoIterator<Item = (String, String, bool)>,
    rename_pairs: Option<&Vec<(String, String)>>,
    add: Option<&Vec<(String, syn::Type)>>,
    at: &impl ErrorAt,
) -> Result<(), syn::Error> {
    let unraw = |name: &str| name.strip_prefix("r#").unwrap_or(name).to_string();
    let renamed: std::collections::HashMap<String, &String> = rename_pairs
        .into_iter()
        .flatten()
        .map(|(_, to)| (unraw(to), to))
        .collect();
    let mut sources: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (name, source, undecided) in kept {
        if undecided {
            continue;
        }
        let name = unraw(&name);
        let source = unraw(&source);
        seen.insert(name.clone());
        if let Some(previous) = sources.insert(name.clone(), source.clone())
            && previous != source
            && let Some(written) = renamed.get(&name)
        {
            return Err(at.error_at(
                "rename_to",
                written,
                format!("`rename` target `{name}` is also the name of another field of the generated type"),
            ));
        }
    }
    let mut written_before: Vec<&String> = Vec::new();
    let mut added: HashSet<String> = HashSet::new();
    for (written, _) in add.into_iter().flatten() {
        let name = unraw(written);
        // The repeated `add` entry, not the first one (L9, round 16).
        let nth = written_before.iter().filter(|w| **w == written).count();
        written_before.push(written);
        // A name listed twice in `add` is not a collision with a source field
        // (R36-SM-6-L1): it is reported as the repeat it is.
        if !added.insert(name.clone()) {
            return Err(at.error_at_occurrence(
                "add",
                written,
                nth,
                format!("`add` lists field `{name}` more than once: keep one entry"),
            ));
        }
        if !seen.insert(name.clone()) {
            return Err(at.error_at_occurrence(
                "add",
                written,
                nth,
                format!("`add` field `{name}` is already a field of the generated type: omit or rename the source field, or pick another name"),
            ));
        }
    }
    Ok(())
}

/// Validates that all fields in `partial` (when specific fields are listed) exist in the source struct.
///
/// Returns an error if any field in `partial` does not exist.
pub fn validate_partial_fields(
    partial_fields: Option<&Vec<String>>,
    source_field_names: &HashSet<String>,
    at: &impl ErrorAt,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    validate_fields_exist(
        "partial",
        partial_fields.into_iter().flatten().map(String::as_str),
        source_field_names,
        at,
        source_type_name,
    )
}

/// Validates `relation_adapters`: every entry names a source field, at most
/// once (a second adapter for the same field used to be silently ignored).
pub fn validate_relation_adapters(
    adapters: &[(String, syn::Ident)],
    source_field_names: &HashSet<String>,
    at: &impl ErrorAt,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    validate_fields_exist(
        "relation_adapters",
        adapters.iter().map(|(field, _)| field.as_str()),
        source_field_names,
        at,
        source_type_name,
    )?;
    let mut seen = HashSet::new();
    for (field, _) in adapters {
        if !seen.insert(field.as_str()) {
            // Spanned at the repeated field name (like `rename`), not the
            // adapter type (nor the first entry).
            return Err(at.error_at_occurrence(
                "relation_adapters",
                field,
                1,
                format!("`relation_adapters` lists field `{field}` more than once"),
            ));
        }
    }
    Ok(())
}

/// Validates that the fields `param` (`rename`, `partial`,
/// `relation_adapters`) names are ones the DTO keeps: naming a field that
/// `omit` drops or `pick` leaves out used to be silently ignored.
pub fn validate_included<'a>(
    param: &str,
    fields: impl IntoIterator<Item = &'a str>,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    at: &impl ErrorAt,
) -> Result<(), syn::Error> {
    for field in fields {
        let reason = if omit_set.contains(field) {
            "`omit` drops it"
        } else if !pick_set.is_empty() && !pick_set.contains(field) {
            "`pick` does not list it"
        } else {
            continue;
        };
        return Err(at.error_at(
            param,
            field,
            format!("`{param}` names field `{field}`, which this type does not include: {reason}"),
        ));
    }
    Ok(())
}

/// Rejects a `param` entry naming a field the type does not emit although
/// `omit` / `pick` keep it (`dropped`: field → reason).
pub fn validate_emitted<'a>(
    param: &str,
    fields: impl IntoIterator<Item = &'a str>,
    dropped: &std::collections::HashMap<String, &str>,
    at: &impl ErrorAt,
) -> Result<(), syn::Error> {
    for field in fields {
        if let Some(reason) = dropped.get(field) {
            return Err(at.error_at(
                param,
                field,
                format!(
                    "`{param}` names field `{field}`, which this type does not include: {reason}"
                ),
            ));
        }
    }
    Ok(())
}

/// Extracts all field names from a struct's named fields.
///
/// This is the set `rename` / `partial` / `relation_adapters` validate against:
/// a field a `#[cfg]` removes in this build is still a name, and naming it is
/// accepted (it has no effect in that build). `pick` / `omit` are the ones that
/// refuse such a name (`field_selection`).
///
/// Returns an empty set for tuple or unit structs.
pub fn extract_source_field_names(parsed_struct: &syn::ItemStruct) -> HashSet<String> {
    use crate::parser::strip_raw_prefix_owned;

    if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
        fields_named
            .named
            .iter()
            .filter_map(|f| f.ident.as_ref())
            .map(|i| strip_raw_prefix_owned(i.to_string()))
            .collect()
    } else {
        HashSet::new()
    }
}

/// The entries the "Available fields" listing shows: the named fields a
/// statically false `#[cfg]` (or `feature` predicate) removes are left out
/// (R28-SM4-L1), and so are the fields serde does not serialize (`pick` /
/// `omit` refuse them, R32-SM5-L1). A field whose serde JSON name differs from
/// its Rust identifier is listed under both spellings as separate entries
/// (R32-SM5-L2), since `pick` / `omit` accept either one on its own. A JSON
/// name that another serialized field's Rust identifier shadows is not
/// selectable (`field_selection::select` picks that identifier's field), so it
/// is listed marked as shadowed rather than as a plain name that would read as
/// the other field's spelling (R33-SM5-V1). Only the listing; validation uses
/// [`extract_source_field_names`].
fn listed_source_field_names(parsed_struct: &syn::ItemStruct) -> HashSet<String> {
    let serialized: Vec<SourceField> = source_fields(parsed_struct)
        .into_iter()
        .filter(|field| field.serialized)
        .collect();
    serialized
        .iter()
        .flat_map(|field| {
            let json = serialized
                .iter()
                // A `#[cfg]` twin shares the Rust ident, so its JSON name is no
                // other field's spelling (R34-SM5-1).
                .find(|other| {
                    other.index != field.index
                        && other.rust != field.rust
                        && other.rust == field.json
                })
                .map_or_else(
                    || field.json.clone(),
                    |shadow| {
                        format!(
                            "{} (JSON name of `{}`, shadowed by field `{}`)",
                            field.json, field.rust, shadow.rust
                        )
                    },
                );
            [field.rust.clone(), json]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    fn create_field_names(names: &[&str]) -> HashSet<String> {
        names.iter().map(std::string::ToString::to_string).collect()
    }

    #[rstest::rstest]
    #[case::camel_json_name(
        "#[serde(rename_all = \"camelCase\")] struct User { user_name: String, id: i32 }",
        "userName",
        Some("user_name")
    )]
    #[case::rust_name("struct User { user_name: String }", "user_name", Some("user_name"))]
    #[case::skipped_is_refused(
        "struct User { id: i32, #[serde(skip)] hidden: i32 }",
        "hidden",
        None
    )]
    #[case::unknown_is_refused("struct User { id: i32 }", "nope", None)]
    fn resolves_filter_names(
        #[case] source: &str,
        #[case] name: &str,
        #[case] expected: Option<&str>,
    ) {
        let parsed: syn::ItemStruct = syn::parse_str(source).unwrap();
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let names = Some(vec![name.to_string()]);
        let result = resolve_filter_names("pick", names.as_ref(), &parsed, "User", &ty);
        match expected {
            Some(rust) => assert_eq!(result.unwrap(), HashSet::from([rust.to_string()])),
            None => assert!(result.is_err()),
        }
    }

    #[test]
    fn json_name_shared_by_two_fields_is_refused() {
        let parsed: syn::ItemStruct = syn::parse_str(
            "struct User { #[serde(rename = \"v\")] x: i32, #[serde(rename = \"v\")] y: i32 }",
        )
        .unwrap();
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let names = Some(vec!["v".to_string()]);
        let err = resolve_filter_names("omit", names.as_ref(), &parsed, "User", &ty)
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than one field"), "{err}");
    }

    #[test]
    fn unknown_name_lists_the_available_fields() {
        let parsed: syn::ItemStruct = syn::parse_str("struct User { id: i32 }").unwrap();
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let names = Some(vec!["missing".to_string()]);
        let err = resolve_filter_names("omit", names.as_ref(), &parsed, "User", &ty)
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("\"id\""), "{err}");
    }

    #[test]
    fn test_validate_rename_fields_success() {
        let source_fields = create_field_names(&["id", "name"]);
        let rename = Some(vec![("id".to_string(), "user_id".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_rename_fields_nonexistent() {
        let source_fields = create_field_names(&["id", "name"]);
        let rename = Some(vec![("missing".to_string(), "new_name".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_validate_rename_fields_invalid_target_ident() {
        // Renaming to a non-identifier ("user-id") must surface as a spanned
        // error, NOT panic the proc-macro at the downstream `syn::Ident::new`.
        let source_fields = create_field_names(&["id", "name"]);
        let rename = Some(vec![("id".to_string(), "user-id".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not a valid"));
        assert!(err.contains("user-id"));
    }

    #[test]
    fn test_validate_rename_fields_keyword_target_rejected() {
        // A reserved keyword target ("type") would emit an uncompilable field
        // and `syn::Ident::new` rejects it — surface a clean error instead.
        let source_fields = create_field_names(&["id"]);
        let rename = Some(vec![("id".to_string(), "type".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_rename_fields_raw_ident_target_ok() {
        // A raw identifier target (`r#type`) is a legal field name and must pass.
        let source_fields = create_field_names(&["id"]);
        let rename = Some(vec![("id".to_string(), "r#type".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    /// R20-S4-L2 (round 20): a raw path keyword (`r#self`) is not a field
    /// name: syn accepts it, but the emitted field would be a plain `self`.
    #[rstest::rstest]
    #[case::raw_self("r#self")]
    #[case::raw_super("r#super")]
    #[case::raw_crate("r#crate")]
    #[case::raw_capital_self("r#Self")]
    #[case::raw_underscore("r#_")]
    fn test_validate_rename_fields_raw_path_keyword_rejected(#[case] target: &str) {
        let source_fields = create_field_names(&["id"]);
        let rename = Some(vec![("id".to_string(), target.to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err(), "`{target}` must not be a field name");
    }

    /// R31-SM5-L1: `syn::parse_str` tokenizes its input, so padded or commented
    /// text parses to a trimmed identifier. The guards must check the text that
    /// is emitted, so these names are refused as a rename target and as an
    /// `add` name (a padded `id` would otherwise collide with `id` unseen).
    #[rstest::rstest]
    #[case::padded(" id ")]
    #[case::commented("id /* c */")]
    #[case::padded_raw_self(" r#self ")]
    fn test_padded_field_names_are_refused(#[case] name: &str) {
        let source_fields = create_field_names(&["id"]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let rename = Some(vec![("id".to_string(), name.to_string())]);
        assert!(
            validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User").is_err(),
            "rename target `{name}`"
        );
        let add = Some(vec![(name.to_string(), syn::parse_quote!(String))]);
        assert!(
            validate_add_field_idents(add.as_ref(), &ty).is_err(),
            "add name `{name}`"
        );
    }

    /// L15: a field renamed twice used to take the last target silently.
    #[test]
    fn test_validate_rename_fields_duplicate_source_rejected() {
        let source_fields = create_field_names(&["id"]);
        let rename = Some(vec![
            ("id".to_string(), "key".to_string()),
            ("id".to_string(), "other".to_string()),
        ]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let err = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User")
            .unwrap_err()
            .to_string();
        assert_eq!(err, "`rename` lists source field `id` more than once");
    }

    #[test]
    fn test_validate_add_field_idents_valid() {
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let add = Some(vec![
            ("extra".to_string(), syn::parse_quote!(String)),
            ("count".to_string(), syn::parse_quote!(i32)),
        ]);
        assert!(validate_add_field_idents(add.as_ref(), &ty).is_ok());
    }

    #[test]
    fn test_validate_add_field_idents_invalid() {
        // An `add` name that is not a valid identifier must error, not panic.
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let add = Some(vec![("bad-name".to_string(), syn::parse_quote!(String))]);
        let result = validate_add_field_idents(add.as_ref(), &ty);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bad-name"));
    }

    #[test]
    fn test_validate_partial_fields_success() {
        let source_fields = create_field_names(&["id", "name", "email"]);
        let partial = Some(vec!["name".to_string(), "email".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_partial_fields(partial.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_partial_fields_nonexistent() {
        let source_fields = create_field_names(&["id", "name"]);
        let partial = Some(vec!["nonexistent".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_partial_fields(partial.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_extract_source_field_names_named() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct User { pub id: i32, pub name: String }").unwrap();
        let names = extract_source_field_names(&struct_def);

        assert!(names.contains("id"));
        assert!(names.contains("name"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn test_extract_source_field_names_tuple() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct Point(pub i32, pub i32);").unwrap();
        let names = extract_source_field_names(&struct_def);

        assert!(names.is_empty());
    }

    #[test]
    fn test_extract_source_field_names_raw_identifier() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct Config { pub r#type: String }").unwrap();
        let names = extract_source_field_names(&struct_def);

        assert!(names.contains("type"));
        assert_eq!(names.len(), 1);
    }

    /// R28-SM4-L1: the "Available fields" listing omits a field a statically
    /// false `cfg` removes.
    #[test]
    fn cfg_disabled_fields_are_left_out_of_the_listing() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct S { #[cfg(any())] gone: i32, id: i32 }").unwrap();
        let ty: syn::Type = syn::parse2(quote!(S)).unwrap();
        let omit = Some(vec!["nope".to_string()]);
        let err = resolve_filter_names("omit", omit.as_ref(), &struct_def, "S", &ty)
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"id\""), "{err}");
        assert!(!err.contains("gone"), "{err}");
    }

    /// R31-SM5-L3 / R32-SM5-L2: a field whose serde JSON name differs from its
    /// Rust identifier is listed under both spellings, each one a valid entry
    /// on its own (`user_name (userName)` was not accepted verbatim).
    #[test]
    fn listing_shows_the_json_name_where_it_differs() {
        let struct_def: syn::ItemStruct = syn::parse_str(
            "#[derive(serde::Serialize)] #[serde(rename_all = \"camelCase\")] pub struct S { user_name: i32, id: i32 }",
        )
        .unwrap();
        let ty: syn::Type = syn::parse2(quote!(S)).unwrap();
        let omit = Some(vec!["nope".to_string()]);
        let err = resolve_filter_names("omit", omit.as_ref(), &struct_def, "S", &ty)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Available fields: [\"id\", \"userName\", \"user_name\"]"),
            "{err}"
        );
    }

    /// R32-SM5-L1: the listing leaves out a field serde does not serialize,
    /// since `pick` / `omit` refuse its name (`serde does not serialize it`).
    #[test]
    fn listing_leaves_out_serde_skipped_fields() {
        let struct_def: syn::ItemStruct = syn::parse_str(
            "#[derive(serde::Serialize)] pub struct S { id: i32, #[serde(skip)] hidden: i32, #[serde(skip_serializing)] quiet: i32 }",
        )
        .unwrap();
        let ty: syn::Type = syn::parse2(quote!(S)).unwrap();
        let omit = Some(vec!["nope".to_string()]);
        let err = resolve_filter_names("omit", omit.as_ref(), &struct_def, "S", &ty)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Available fields: [\"id\"]"), "{err}");
        // The skipped name is still refused with its own diagnostic.
        let hidden = Some(vec!["hidden".to_string()]);
        let err = resolve_filter_names("omit", hidden.as_ref(), &struct_def, "S", &ty)
            .unwrap_err()
            .to_string();
        assert!(err.contains("serde does not serialize it"), "{err}");
    }

    /// R33-SM5-V1: `a` is serialized as `b`, but `b` names field `b` (the
    /// Rust identifier wins), so the JSON name `b` of `a` is marked as
    /// shadowed in the listing instead of reading as a plain `b`. `z` (the
    /// JSON name of `b`) is an ordinary entry.
    #[test]
    fn listing_marks_a_json_name_shadowed_by_another_rust_ident() {
        let struct_def: syn::ItemStruct = syn::parse_str(
            "#[derive(serde::Serialize)] pub struct S { #[serde(rename = \"b\")] a: i32, #[serde(rename = \"z\")] b: i32 }",
        )
        .unwrap();
        let ty: syn::Type = syn::parse2(quote!(S)).unwrap();
        let omit = Some(vec!["nope".to_string()]);
        let err = resolve_filter_names("omit", omit.as_ref(), &struct_def, "S", &ty)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                "Available fields: [\"a\", \"b\", \"b (JSON name of `a`, shadowed by field `b`)\", \"z\"]"
            ),
            "{err}"
        );
        // The selection itself is unchanged: `b` is field `b`, `z` is its JSON
        // name, and `a` (the field the marked entry names) is still `a`.
        let picked = Some(vec!["b".to_string()]);
        let rust = resolve_filter_names("pick", picked.as_ref(), &struct_def, "S", &ty).unwrap();
        assert_eq!(rust, HashSet::from(["b".to_string()]));
    }

    /// R28-SM4-L2 (decided: accepted). A `rename` / `partial` / adapter name of
    /// a `#[cfg(feature = "x")]` field is accepted with the feature off: a DTO
    /// may rename a feature-gated field, and the rename has no effect in that
    /// build. `pick` / `omit` still refuse it (`field_selection`).
    #[test]
    fn cfg_removed_field_name_is_accepted_by_rename_and_partial() {
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--crate-name\napp",
        );
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct S { #[cfg(feature = \"x\")] gated: i32, id: i32 }").unwrap();
        // R29-SM4-02: the premise is checked, not assumed: the field is
        // statically removed in this build (the `x` feature is off).
        let gated = struct_def
            .fields
            .iter()
            .find(|field| field.ident.as_ref().is_some_and(|ident| *ident == "gated"))
            .unwrap();
        assert!(
            crate::schema_macro::cfg_eval::statically_inactive(&gated.attrs),
            "`gated` must be cfg-removed"
        );
        let ty: syn::Type = syn::parse2(quote!(S)).unwrap();
        let fields = extract_source_field_names(&struct_def);
        let rename = Some(vec![("gated".to_string(), "other".to_string())]);
        assert!(validate_rename_fields(rename.as_ref(), &fields, &ty, "S").is_ok());
        let partial = Some(vec!["gated".to_string()]);
        assert!(validate_partial_fields(partial.as_ref(), &fields, &ty, "S").is_ok());
    }

    /// A `rename` target / `add` name equal to another generated field is a
    /// named error; same-named kept source fields (`#[cfg]` twins) are not.
    #[rstest::rstest]
    #[case::rename_onto_kept(&[("name", "name"), ("name", "id")], &[("id", "name")], &[], Some("`rename` target `name`"))]
    #[case::two_renames_one_target(&[("x", "a"), ("x", "b")], &[("a", "x"), ("b", "x")], &[], Some("`rename` target `x`"))]
    #[case::add_onto_kept(&[("id", "id"), ("name", "name")], &[], &["id"], Some("`add` field `id`"))]
    #[case::raw_add_onto_kept(&[("r#type", "r#type")], &[], &["type"], Some("`add` field `type`"))]
    #[case::cfg_twins(&[("id", "id"), ("id", "id")], &[], &[], None)]
    // Renamed `#[cfg]` twins map to one target from one source name: they
    // used to be rejected as a duplicate.
    #[case::renamed_cfg_twins(&[("key", "id"), ("key", "id")], &[("id", "key")], &[], None)]
    #[case::distinct(&[("id", "id"), ("label", "name")], &[("name", "label")], &["extra"], None)]
    // R36-SM-6-L1: a name listed twice in `add` is the repeat, not a collision.
    #[case::add_twice(&[("id", "id")], &[], &["extra", "extra"], Some("`add` lists field `extra` more than once"))]
    fn unique_field_names(
        #[case] kept: &[(&str, &str)],
        #[case] renames: &[(&str, &str)],
        #[case] add: &[&str],
        #[case] error: Option<&str>,
    ) {
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let renames: Vec<(String, String)> = renames
            .iter()
            .map(|(from, to)| ((*from).to_string(), (*to).to_string()))
            .collect();
        let add: Vec<(String, syn::Type)> = add
            .iter()
            .map(|name| ((*name).to_string(), syn::parse_quote!(i32)))
            .collect();
        let result = validate_unique_field_names(
            kept.iter()
                .map(|(name, source)| ((*name).to_string(), (*source).to_string(), false)),
            Some(&renames),
            Some(&add),
            &ty,
        );
        match error {
            None => assert!(result.is_ok()),
            Some(message) => {
                let err = result.unwrap_err().to_string();
                assert!(err.contains(message), "{err}");
            }
        }
    }

    /// R35-VAL-1: an `add` field named like an undecided `#[cfg]` field is
    /// accepted (the build keeping both gets E0124 from rustc); a decided one
    /// is refused.
    #[rstest::rstest]
    #[case::undecided_cfg_field(true, true)]
    #[case::decided_field(false, false)]
    fn add_name_of_an_undecided_field(#[case] undecided: bool, #[case] accepted: bool) {
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let add = vec![("x".to_string(), syn::parse_quote!(i64))];
        let result = validate_unique_field_names(
            [("x".to_string(), "x".to_string(), undecided)],
            None,
            Some(&add),
            &ty,
        );
        assert_eq!(result.is_ok(), accepted);
    }

    /// R34-SM5-2: a rename target that only an undecided `#[cfg]` field also
    /// emits is not refused here (the build keeping both gets rustc's E0124); a
    /// definite collision still is.
    #[rstest::rstest]
    #[case::undecided_cfg_field(&[("id", "a", false), ("id", "id", true)], true)]
    #[case::definite_collision(&[("id", "a", false), ("id", "id", false)], false)]
    fn rename_target_of_an_undecided_field(
        #[case] kept: &[(&str, &str, bool)],
        #[case] accepted: bool,
    ) {
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let renames = vec![("a".to_string(), "id".to_string())];
        let result = validate_unique_field_names(
            kept.iter().map(|(name, source, undecided)| {
                ((*name).to_string(), (*source).to_string(), *undecided)
            }),
            Some(&renames),
            None,
            &ty,
        );
        assert_eq!(result.is_ok(), accepted);
    }

    /// R34-SM5-1: a `#[cfg]` twin's JSON name (its own Rust ident) is listed
    /// plainly, not as shadowed by its sibling twin. Both twins are active
    /// here, so the `other.rust != field.rust` guard is what keeps `id` from
    /// reading as shadowed.
    #[test]
    fn cfg_twin_is_not_listed_as_shadowed() {
        let parsed: syn::ItemStruct =
            syn::parse_str("struct User { #[cfg(all())] id: i32, #[cfg(all())] id: i64 }").unwrap();
        assert_eq!(
            listed_source_field_names(&parsed),
            HashSet::from(["id".to_string()])
        );
    }
}
