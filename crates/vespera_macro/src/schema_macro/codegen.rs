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
use vespera_core::schema::Schema;

use super::SchemaInput;
use crate::{
    metadata::StructMetadata,
    parser::{
        extract_field_rename, extract_rename_all, extract_skip, parse_struct_to_schema,
        rename_field, strip_raw_prefix_owned,
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
    // `schema!` filters the fields first (omitted / unpicked ones matched
    // against BOTH the Rust identifier and the serde-renamed JSON name).
    let filtered = filter_struct_fields(struct_item, omit_set, pick_set);
    parse_struct_to_schema(
        filtered.as_ref().unwrap_or(struct_item),
        &known_schemas,
        &struct_definitions,
    )
}

/// One field the schema carries (not `#[serde(skip)]`ped).
struct EmittedField {
    rust: String,
    json: String,
}

/// A field's Rust identifier (unraw) and its JSON name.
fn field_names(field: &syn::Field, rename_all: Option<&str>) -> (String, String) {
    let rust = field.ident.as_ref().map_or_else(
        || "unknown".to_string(),
        |i| strip_raw_prefix_owned(i.to_string()),
    );
    let json =
        extract_field_rename(&field.attrs).unwrap_or_else(|| rename_field(&rust, rename_all));
    (rust, json)
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
            let (rust, json) = field_names(field, rename_all.as_deref());
            EmittedField { rust, json }
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

/// `struct_item` without the fields `omit` / `pick` remove, or `None` when
/// no filtering is requested (both sets empty → keep every field).
///
/// A name matches its Rust identifier or its serde-renamed JSON name; a
/// `#[serde(flatten)]` field is kept or dropped as a whole. Filtering the
/// struct BEFORE the schema is built keeps the schema exactly what the
/// filtered struct documents, whatever shape the flatten composition takes
/// (filtering the built schema assumed `allOf: [own, one per flattened
/// field]`, which merged flatten members no longer follow).
fn filter_struct_fields(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
) -> Option<syn::ItemStruct> {
    if omit_set.is_empty() && pick_set.is_empty() {
        return None;
    }
    let mut filtered = struct_item.clone();
    if let syn::Fields::Named(fields_named) = &mut filtered.fields {
        let rename_all = extract_rename_all(&struct_item.attrs);
        fields_named.named = std::mem::take(&mut fields_named.named)
            .into_iter()
            .filter(|field| {
                let (rust, json) = field_names(field, rename_all.as_deref());
                let named = |set: &HashSet<String>| set.contains(&rust) || set.contains(&json);
                !named(omit_set) && (pick_set.is_empty() || named(pick_set))
            })
            .collect();
    }
    Some(filtered)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use crate::metadata::StructMetadata;

    use super::{
        SchemaInput, build_filtered_schema, filter_struct_fields, generate_filtered_schema,
        validate_filter_names,
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
    fn filter_struct_fields_matches_rust_and_json_names() {
        let mut item = parse(
            r#"#[serde(rename_all = "camelCase")] pub struct S { pub user_name: String, pub r#type: i32, pub other: i32 }"#,
        );
        assert!(filter_struct_fields(&item, &HashSet::new(), &HashSet::new()).is_none());
        let pick = HashSet::from(["userName".to_string(), "type".to_string()]);
        let filtered = filter_struct_fields(&item, &HashSet::new(), &pick).unwrap();
        let names: Vec<String> = filtered
            .fields
            .iter()
            .map(|f| f.ident.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(names, ["user_name", "r#type"]);

        // An identifierless field is named `unknown`; a tuple struct has
        // nothing to filter.
        let syn::Fields::Named(fields) = &mut item.fields else {
            panic!("fixture is a named struct");
        };
        fields.named[2].ident = None;
        let omit = HashSet::from(["unknown".to_string()]);
        let filtered = filter_struct_fields(&item, &omit, &HashSet::new()).unwrap();
        assert_eq!(filtered.fields.len(), 2);
        let tuple = parse("pub struct T(pub i32);");
        let filtered = filter_struct_fields(&tuple, &omit, &HashSet::new()).unwrap();
        assert_eq!(filtered.fields.len(), 1);
    }

    /// M10: an `Option` flatten member is merged into the own object; the
    /// post-build filter took it for an `allOf` member and kept its
    /// properties when the flattened field was omitted.
    #[test]
    fn filtering_a_merged_flatten_member() {
        let item = parse(
            "pub struct S { pub id: i32, pub name: String, #[serde(flatten)] pub base: Option<Base> }",
        );
        let full =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &flatten_storage());
        let keys = |schema: &super::Schema| {
            schema
                .properties
                .as_ref()
                .map(|p| p.keys().cloned().collect::<Vec<_>>())
        };
        assert_eq!(
            keys(&full),
            Some(vec!["created".into(), "id".into(), "name".into()])
        );

        let omit = HashSet::from(["base".to_string()]);
        let schema = build_filtered_schema(&item, &omit, &HashSet::new(), &flatten_storage());
        assert_eq!(keys(&schema), Some(vec!["id".into(), "name".into()]));

        let pick = HashSet::from(["base".to_string()]);
        let schema = build_filtered_schema(&item, &HashSet::new(), &pick, &flatten_storage());
        assert_eq!(keys(&schema), Some(vec!["created".into()]));
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
        let vespera_core::schema::SchemaRef::Inline(own) = &all_of[0] else {
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
