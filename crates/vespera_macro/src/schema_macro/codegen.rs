//! Code generation for the `schema!` macro.
//!
//! `schema!(Type, pick/omit)` must return a runtime [`Schema`] that is
//! **identical** to the OpenAPI component schema generated for `Type`.
//! To guarantee that, this module does NOT re-implement schema
//! construction: it calls the shared [`parse_struct_to_schema`] path (the
//! single source of truth the OpenAPI generator also uses), applies the
//! `pick`/`omit` field filter, serializes the resulting [`Schema`] to JSON
//! at compile time, and emits a [`Schema::from_compiled_json`] call.  The
//! runtime value is reconstructed byte-for-byte from that spec, so
//! `schema!` can never drift from the documented component schema
//! (required-by-nullability, doc descriptions, `flatten`/`transparent`
//! composition, field constraints, and `$ref` references).

use std::collections::{HashMap, HashSet};

use proc_macro2::TokenStream;
use quote::quote;
use vespera_core::schema::{Schema, SchemaRef};

use super::SchemaInput;
use crate::{
    metadata::StructMetadata,
    parser::{
        extract_field_rename, extract_rename_all, extract_skip, parse_struct_to_schema,
        rename_field, schema::extract_flatten, strip_raw_prefix_owned,
    },
};

/// Generate a `schema!` expression: a runtime [`Schema`] identical to the
/// OpenAPI component schema for `struct_item`, after applying `pick`/`omit`.
///
/// The schema is built through the shared [`parse_struct_to_schema`] path,
/// serialized at compile time, and reconstructed at runtime via
/// [`Schema::from_compiled_json`] — so the `schema!` result and the
/// generated OpenAPI component schema can never diverge.
pub fn generate_filtered_schema(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    schema_storage: &HashMap<String, StructMetadata>,
) -> TokenStream {
    let schema = build_filtered_schema(struct_item, omit_set, pick_set, schema_storage);
    // Serialize at compile time; the runtime value is reconstructed from
    // this spec so it cannot diverge from the OpenAPI component schema.
    let json = serde_json::to_string(&schema).expect("Schema serialization is infallible");
    quote! {
        vespera::schema::Schema::from_compiled_json(#json)
    }
}

/// Build the filtered [`Schema`] value for `schema!` — the OpenAPI
/// component schema for `struct_item` with the `pick`/`omit` field filter
/// applied.
///
/// Split out from [`generate_filtered_schema`] so the filtering semantics
/// are unit-testable on the produced value (rather than on the emitted
/// token string).
fn build_filtered_schema(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    schema_storage: &HashMap<String, StructMetadata>,
) -> Schema {
    // Same resolution context the OpenAPI component path builds: every
    // known schema name (for `$ref` resolution) and its source definition
    // (for generic expansion).
    let known_schemas: HashSet<&str> = schema_storage.keys().map(String::as_str).collect();
    let struct_definitions: HashMap<&str, &str> = schema_storage
        .values()
        .map(|s| (s.name.as_str(), s.definition.as_str()))
        .collect();

    // Single source of truth — identical logic to OpenAPI generation
    // (required-by-nullability, doc descriptions, flatten/transparent,
    // field constraints, `$ref` references).
    let mut schema = parse_struct_to_schema(struct_item, &known_schemas, &struct_definitions);

    // `schema!` layers field filtering on top: drop the omitted / unpicked
    // properties (matched against BOTH the Rust identifier and the
    // serde-renamed JSON name, as the prior hand-rolled walk did).
    if let Some(filter) = compute_field_filter(struct_item, omit_set, pick_set) {
        filter_schema_fields(&mut schema, &filter);
    }

    schema
}

/// One field the schema carries (not `#[serde(skip)]`ped).
struct EmittedField {
    rust: String,
    json: String,
    flatten: bool,
}

/// The fields of `struct_item` its schema carries, in declaration order.
fn emitted_fields(struct_item: &syn::ItemStruct) -> Vec<EmittedField> {
    let syn::Fields::Named(fields_named) = &struct_item.fields else {
        return Vec::new();
    };
    let rename_all = extract_rename_all(&struct_item.attrs);
    fields_named
        .named
        .iter()
        .filter(|field| !extract_skip(&field.attrs))
        .map(|field| {
            let rust = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |i| strip_raw_prefix_owned(i.to_string()),
            );
            let json = extract_field_rename(&field.attrs)
                .unwrap_or_else(|| rename_field(&rust, rename_all.as_deref()));
            EmittedField {
                rust,
                json,
                flatten: extract_flatten(&field.attrs),
            }
        })
        .collect()
}

/// Validates that every `omit` / `pick` name is a field `ty`'s schema
/// carries (Rust identifier or serde JSON name): a typo used to filter
/// nothing (`omit`) or everything (`pick`) without a diagnostic.
pub fn validate_filter_names(
    struct_item: &syn::ItemStruct,
    input: &SchemaInput,
    type_name: &str,
) -> Result<(), syn::Error> {
    let fields = emitted_fields(struct_item);
    for (param, names) in [("omit", &input.omit), ("pick", &input.pick)] {
        for name in names.iter().flatten() {
            if fields.iter().any(|f| &f.rust == name || &f.json == name) {
                continue;
            }
            let available: Vec<&str> = fields.iter().map(|f| f.rust.as_str()).collect();
            let message = format!(
                "field `{name}` does not exist in the schema of `{type_name}`. Available fields: {available:?}"
            );
            return Err(match input.spans.field(param, name) {
                Some(span) => syn::Error::new(span, message),
                None => syn::Error::new_spanned(&input.ty, message),
            });
        }
    }
    Ok(())
}

/// What the `pick` / `omit` filter removes.
#[derive(Debug, PartialEq, Eq)]
struct FieldFilter {
    /// JSON names of the dropped own properties.
    dropped: HashSet<String>,
    /// Per `#[serde(flatten)]` field (declaration order): whether it stays.
    flattened: Vec<bool>,
}

/// The filter `omit` / `pick` describe, or `None` when no filtering is
/// requested (both sets empty → keep every field).
///
/// Mirrors the OpenAPI field walk: `#[serde(skip)]` fields never qualify,
/// and a name matches `omit`/`pick` against EITHER its Rust identifier or
/// its serde-renamed JSON name. A `#[serde(flatten)]` field is kept or
/// dropped as a whole (its `allOf` member).
fn compute_field_filter(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
) -> Option<FieldFilter> {
    if omit_set.is_empty() && pick_set.is_empty() {
        return None;
    }
    let mut filter = FieldFilter {
        dropped: HashSet::new(),
        flattened: Vec::new(),
    };
    for field in emitted_fields(struct_item) {
        let named = |set: &HashSet<String>| set.contains(&field.rust) || set.contains(&field.json);
        let kept = !named(omit_set) && (pick_set.is_empty() || named(pick_set));
        if field.flatten {
            filter.flattened.push(kept);
        } else if !kept {
            filter.dropped.insert(field.json);
        }
    }
    Some(filter)
}

/// Applies `filter` to `schema`. With flattened fields the schema is
/// `allOf: [<own properties>, ...flattened]`: the own member is filtered,
/// dropped flattened members are removed, and a lone remaining own member
/// becomes the schema itself (as [`parse_struct_to_schema`] represents a
/// struct without flattened fields). Filtering the top level of an `allOf`
/// schema used to be a no-op.
fn filter_schema_fields(schema: &mut Schema, filter: &FieldFilter) {
    let Some(all_of) = schema
        .all_of
        .as_mut()
        .filter(|_| !filter.flattened.is_empty())
    else {
        retain_properties(schema, &filter.dropped);
        return;
    };
    let mut kept = std::iter::once(true).chain(filter.flattened.iter().copied());
    all_of.retain(|_| kept.next().unwrap_or(true));
    if let Some(SchemaRef::Inline(own)) = all_of.first_mut() {
        retain_properties(own, &filter.dropped);
    }
    if let [SchemaRef::Inline(own)] = all_of.as_slice() {
        *schema = Schema {
            description: schema.description.take(),
            ..(**own).clone()
        };
    }
}

/// Removes the `dropped` properties (and their `required` entries),
/// normalizing an emptied `properties`/`required` back to `None` to match
/// [`parse_struct_to_schema`]'s own representation. Properties no field
/// declares (a struct `#[serde(tag)]`) stay.
fn retain_properties(schema: &mut Schema, dropped: &HashSet<String>) {
    if let Some(properties) = &mut schema.properties {
        properties.retain(|name, _| !dropped.contains(name));
        if properties.is_empty() {
            schema.properties = None;
        }
    }
    if let Some(required) = &mut schema.required {
        required.retain(|name| !dropped.contains(name));
        if required.is_empty() {
            schema.required = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use crate::metadata::StructMetadata;

    use super::{
        FieldFilter, SchemaInput, build_filtered_schema, compute_field_filter,
        generate_filtered_schema, validate_filter_names,
    };

    fn empty_storage() -> HashMap<String, StructMetadata> {
        HashMap::new()
    }

    fn parse(src: &str) -> syn::ItemStruct {
        syn::parse_str(src).expect("valid struct source")
    }

    /// Regression for the schema!↔OpenAPI drift: a `#[serde(default)]`
    /// non-`Option` field must be REQUIRED (required is nullability-only,
    /// identical to the OpenAPI component schema).  The prior `schema!`
    /// path wrongly excluded defaulted fields.
    #[test]
    fn default_field_is_required_matching_openapi() {
        let item = parse(r"pub struct WithDefault { #[serde(default)] pub field: String }");
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        let required = schema.required.expect("required set present");
        assert!(
            required.iter().any(|f| f == "field"),
            "a defaulted non-Option field must be required, got {required:?}"
        );
    }

    /// A `skip_serializing_if` / `skip_serializing` field may be absent from
    /// serialized output, so it is NOT required (shared struct-field walker).
    #[rstest::rstest]
    #[case::skip_serializing_if(r#"#[serde(skip_serializing_if = "String::is_empty")]"#)]
    #[case::skip_serializing("#[serde(skip_serializing)]")]
    fn output_omittable_field_is_not_required(#[case] attr: &str) {
        let item = parse(&format!(
            "pub struct WithSkip {{ {attr} pub field: String, pub id: i32 }}"
        ));
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        let required = schema.required.expect("required present");
        assert_eq!(required, ["id"], "{attr}");
        assert!(schema.properties.expect("props").contains_key("field"));
    }

    #[test]
    fn option_field_is_not_required() {
        let item = parse(r"pub struct WithOpt { pub field: Option<String> }");
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        let still_required = schema
            .required
            .as_ref()
            .is_some_and(|r| r.iter().any(|f| f == "field"));
        assert!(!still_required, "an Option<T> field must not be required");
    }

    #[test]
    fn omit_excludes_field_from_properties_and_required() {
        let item = parse(r"pub struct S { pub a: String, pub b: i32 }");
        let mut omit = HashSet::new();
        omit.insert("b".to_string());
        let schema = build_filtered_schema(&item, &omit, &HashSet::new(), &empty_storage());
        let props = schema.properties.expect("properties present");
        assert!(props.contains_key("a"));
        assert!(!props.contains_key("b"), "omitted field must be gone");
        assert!(
            !schema.required.unwrap_or_default().iter().any(|f| f == "b"),
            "omitted field must not remain required"
        );
    }

    #[test]
    fn pick_keeps_only_selected_fields() {
        let item = parse(r"pub struct S { pub a: String, pub b: i32, pub c: bool }");
        let mut pick = HashSet::new();
        pick.insert("a".to_string());
        let schema = build_filtered_schema(&item, &HashSet::new(), &pick, &empty_storage());
        let props = schema.properties.expect("properties present");
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("a"));
    }

    #[test]
    fn serde_skip_field_excluded() {
        let item = parse(r"pub struct S { pub a: String, #[serde(skip)] pub hidden: i32 }");
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        let props = schema.properties.expect("properties present");
        assert!(props.contains_key("a"));
        assert!(!props.contains_key("hidden"), "serde(skip) field excluded");
    }

    #[test]
    fn pick_matches_renamed_json_name() {
        let item = parse(
            r#"#[serde(rename_all = "camelCase")] pub struct S { pub user_name: String, pub age: i32 }"#,
        );
        let mut pick = HashSet::new();
        pick.insert("userName".to_string());
        let schema = build_filtered_schema(&item, &HashSet::new(), &pick, &empty_storage());
        let props = schema.properties.expect("properties present");
        assert!(props.contains_key("userName"));
        assert!(!props.contains_key("age"));
    }

    #[test]
    fn omit_matches_rust_name_even_when_renamed() {
        let item = parse(
            r#"#[serde(rename_all = "camelCase")] pub struct S { pub user_name: String, pub age: i32 }"#,
        );
        let mut omit = HashSet::new();
        omit.insert("user_name".to_string()); // Rust identifier, not the JSON name
        let schema = build_filtered_schema(&item, &omit, &HashSet::new(), &empty_storage());
        let props = schema.properties.expect("properties present");
        assert!(!props.contains_key("userName"), "omit by Rust name works");
        assert!(props.contains_key("age"));
    }

    #[test]
    fn empty_struct_has_no_properties() {
        let item = parse("pub struct Empty {}");
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        assert!(schema.properties.is_none());
    }

    #[test]
    fn tuple_struct_produces_no_properties() {
        let item = parse("pub struct Tuple(i32, String);");
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        assert!(schema.properties.is_none());
    }

    #[test]
    fn generate_emits_from_compiled_json_call() {
        let item = parse(r"pub struct S { pub a: String }");
        let output =
            generate_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage())
                .to_string();
        assert!(
            output.contains("from_compiled_json"),
            "schema! must emit a from_compiled_json reconstruction, got: {output}"
        );
        // The serialized spec carries the property + required set.
        assert!(output.contains("properties"), "spec must carry properties");
        assert!(output.contains("required"), "spec must carry required");
    }

    #[test]
    fn filtering_every_field_normalizes_empty_properties_and_required() {
        let item = parse("pub struct S { pub only: String }");
        let omit = HashSet::from(["only".to_string()]);
        let schema = build_filtered_schema(&item, &omit, &HashSet::new(), &empty_storage());

        assert!(schema.properties.is_none());
        assert!(schema.required.is_none());
    }

    #[test]
    fn filtering_skips_serde_hidden_fields_and_handles_identifierless_field() {
        let mut item = parse(
            "pub struct S { #[serde(skip)] pub hidden: String, pub shown: i32, pub other: i32 }",
        );
        let pick = HashSet::from(["shown".to_string()]);
        let filter = compute_field_filter(&item, &HashSet::new(), &pick)
            .expect("a pick filter always produces a filter");
        assert_eq!(
            filter,
            FieldFilter {
                dropped: HashSet::from(["other".to_string()]),
                flattened: Vec::new(),
            }
        );

        let syn::Fields::Named(fields) = &mut item.fields else {
            panic!("fixture is a named struct");
        };
        fields.named[1].ident = None;
        let filter = compute_field_filter(&item, &HashSet::new(), &pick).unwrap();
        assert_eq!(
            filter.dropped,
            HashSet::from(["unknown".to_string(), "other".to_string()])
        );
    }

    fn flatten_storage() -> HashMap<String, StructMetadata> {
        HashMap::from([(
            "Base".to_string(),
            StructMetadata::new(
                "Base".to_string(),
                "pub struct Base { pub created: i64 }".to_string(),
            ),
        )])
    }

    const FLATTENED: &str =
        "/// Doc\npub struct S { pub id: i32, pub name: String, #[serde(flatten)] pub base: Base }";

    /// M10: with a `#[serde(flatten)]` field the schema is an `allOf`; the
    /// filter used to touch only its (absent) top-level properties.
    #[test]
    fn filtering_reaches_into_flattened_all_of() {
        let item = parse(FLATTENED);
        // `pick` without the flattened field: one own member is left, which
        // becomes the schema itself (keeping the struct description).
        let pick = HashSet::from(["id".to_string()]);
        let schema = build_filtered_schema(&item, &HashSet::new(), &pick, &flatten_storage());
        assert!(schema.all_of.is_none(), "{schema:?}");
        assert_eq!(schema.description.as_deref(), Some("Doc"));
        let properties = schema.properties.expect("own properties");
        assert_eq!(properties.keys().collect::<Vec<_>>(), ["id"]);
        assert_eq!(schema.required, Some(vec!["id".to_string()]));

        // `omit` of an own field keeps the flattened member.
        let omit = HashSet::from(["name".to_string()]);
        let schema = build_filtered_schema(&item, &omit, &HashSet::new(), &flatten_storage());
        let all_of = schema.all_of.expect("still composed");
        assert_eq!(all_of.len(), 2);
        let super::SchemaRef::Inline(own) = &all_of[0] else {
            panic!("own member is inline");
        };
        assert_eq!(
            own.properties.as_ref().unwrap().keys().collect::<Vec<_>>(),
            ["id"]
        );
    }

    /// Properties no field declares (a struct `#[serde(tag)]`) survive.
    #[test]
    fn filtering_keeps_the_struct_tag() {
        let item =
            parse(r#"#[serde(tag = "kind")] pub struct S { pub id: i32, pub name: String }"#);
        let pick = HashSet::from(["id".to_string()]);
        let schema = build_filtered_schema(&item, &HashSet::new(), &pick, &empty_storage());
        assert_eq!(
            schema.properties.unwrap().keys().collect::<Vec<_>>(),
            ["id", "kind"]
        );
    }

    #[rstest::rstest]
    #[case::rust_name(r#"S, pick = ["user_name"]"#, None)]
    #[case::json_name(r#"S, omit = ["userName"]"#, None)]
    #[case::unknown_omit(
        r#"S, omit = ["nmae"]"#,
        Some(
            "field `nmae` does not exist in the schema of `S`. Available fields: [\"user_name\"]"
        )
    )]
    #[case::skipped_pick(
        r#"S, pick = ["hidden"]"#,
        Some(
            "field `hidden` does not exist in the schema of `S`. Available fields: [\"user_name\"]"
        )
    )]
    fn validates_filter_names(#[case] input: &str, #[case] expected: Option<&str>) {
        let item = parse(
            r#"#[serde(rename_all = "camelCase")] pub struct S { pub user_name: String, #[serde(skip)] pub hidden: i32 }"#,
        );
        let input: SchemaInput = syn::parse_str(input).unwrap();
        let result = validate_filter_names(&item, &input, "S");
        assert_eq!(result.err().map(|e| e.to_string()).as_deref(), expected);
    }

    /// An input built without recorded spans reports at the type; a tuple
    /// struct has no nameable fields.
    #[test]
    fn validate_filter_names_without_spans_and_on_tuple_structs() {
        let input = SchemaInput {
            ty: syn::parse_str("S").unwrap(),
            omit: Some(vec!["x".to_string()]),
            pick: None,
            spans: crate::schema_macro::input::ParamSpans::default(),
        };
        let err = validate_filter_names(&parse("pub struct S(pub i32);"), &input, "S").unwrap_err();
        assert_eq!(
            err.to_string(),
            "field `x` does not exist in the schema of `S`. Available fields: []"
        );
    }
}
