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

use std::collections::{BTreeMap, HashMap, HashSet};

use proc_macro2::TokenStream;
use quote::quote;
use vespera_core::schema::Schema;

use super::SchemaInput;
use super::field_selection::{Refusal, SourceField, resolve_name, select, source_fields};
use crate::{metadata::StructMetadata, parser::parse_struct_to_schema};

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
    field_defaults: &BTreeMap<String, serde_json::Value>,
    source_module: &[String],
) -> TokenStream {
    let mut schema = build_filtered_schema(struct_item, omit_set, pick_set, schema_storage);
    // The same `default` post-processing the component schema gets (the
    // derive's pre-extracted `field_defaults`, `#[schema(default)]`, type
    // defaults); filtered-out fields have no property to receive one.
    crate::openapi_generator::process_default_functions_for(
        struct_item,
        None,
        &mut schema,
        field_defaults,
        crate::parser::schema::SerdeSide::Serialize,
        source_module,
    );
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
    // The resolution context the OpenAPI component path builds, from the
    // same helper: every known schema name (for `$ref` resolution), its
    // source definition (for generic expansion) AND the aliases of renamed
    // types (`#[schema(name = "UserDto")] struct User` is named `User` in
    // Rust). Sorted, so the lookups never depend on storage order.
    let mut metadata = crate::metadata::CollectedMetadata::new();
    metadata.structs = schema_storage.values().cloned().collect();
    metadata.structs.sort_by(|a, b| a.name.cmp(&b.name));

    // Single source of truth — identical logic to OpenAPI generation
    // (required-by-nullability, doc descriptions, flatten/transparent,
    // field constraints, `$ref` references).
    // `schema!` filters the fields first (omitted / unpicked ones matched by
    // Rust identifier, or by the serde-renamed JSON name when no identifier
    // matches).
    let filtered = filter_struct_fields(struct_item, omit_set, pick_set);
    crate::openapi_generator::with_schema_lookups(&metadata, |known_schemas, struct_definitions| {
        parse_struct_to_schema(
            filtered.as_ref().unwrap_or(struct_item),
            known_schemas,
            struct_definitions,
        )
    })
}

/// The fields of `struct_item` its schema carries, in declaration order. The
/// schema is the serialize view: a `skip` or `skip_serializing` field is not
/// in it, whatever its `skip_deserializing` (R25-SM1-L3), and a field a
/// statically false `#[cfg]` removes is in no build (M1, round 27).
fn emitted_fields(struct_item: &syn::ItemStruct) -> Vec<SourceField> {
    source_fields(struct_item)
        .into_iter()
        .filter(|field| field.serialized)
        .collect()
}

/// Validates that every `omit` / `pick` name is a field `ty`'s schema
/// carries (Rust identifier or serde JSON name): a typo used to filter
/// nothing (`omit`) or everything (`pick`) without a diagnostic. A JSON name
/// shared by fields with different Rust identifiers is refused as ambiguous
/// (see [`resolve_name`]).
pub fn validate_filter_names(
    struct_item: &syn::ItemStruct,
    input: &SchemaInput,
    type_name: &str,
) -> Result<(), syn::Error> {
    for (param, names) in [("omit", &input.omit), ("pick", &input.pick)] {
        for name in names.iter().flatten() {
            let message = match resolve_name(struct_item, name) {
                Ok(_) => continue,
                Err(Refusal::Missing { .. }) => {
                    let available: Vec<String> = emitted_fields(struct_item)
                        .into_iter()
                        .map(|field| field.rust)
                        .collect();
                    format!(
                        "field `{name}` does not exist in the schema of `{type_name}`. Available fields: {available:?}"
                    )
                }
                Err(Refusal::Ambiguous(owners)) => format!(
                    "`{name}` is the JSON name of more than one field of `{type_name}` ({owners:?}); name the field by its Rust identifier"
                ),
            };
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
/// A name selects a field by its Rust identifier, or else by its serde-renamed
/// JSON name (see [`selected_fields`]); only the emitted fields are selectable.
/// A `#[serde(flatten)]` field is kept or dropped as a whole. Filtering the
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
        let fields = emitted_fields(struct_item);
        let selected = |set: &HashSet<String>| -> HashSet<usize> {
            set.iter()
                .flat_map(|name| select(&fields, name))
                .map(|f| f.index)
                .collect()
        };
        let (omitted, picked) = (selected(omit_set), selected(pick_set));
        // Only the emitted fields stay: a `cfg` twin the build removes must not
        // reach the schema through the struct this returns.
        let emitted: HashSet<usize> = fields.iter().map(|f| f.index).collect();
        fields_named.named = std::mem::take(&mut fields_named.named)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| {
                emitted.contains(index)
                    && !omitted.contains(index)
                    && (pick_set.is_empty() || picked.contains(index))
            })
            .map(|(_, field)| field)
            .collect();
    }
    Some(filtered)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, HashSet};

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

    /// A `skip_serializing_if` field may be absent from serialized output,
    /// so it is NOT required; a `skip_serializing` one is never written, so
    /// it is not a property at all (shared struct-field walker).
    #[rstest::rstest]
    #[case::skip_serializing_if(r#"#[serde(skip_serializing_if = "String::is_empty")]"#, true)]
    #[case::skip_serializing("#[serde(skip_serializing)]", false)]
    fn output_omittable_field_is_not_required(#[case] attr: &str, #[case] present: bool) {
        let item = parse(&format!(
            "pub struct WithSkip {{ {attr} pub field: String, pub id: i32 }}"
        ));
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        let required = schema.required.expect("required present");
        assert_eq!(required, ["id"], "{attr}");
        assert_eq!(
            schema.properties.expect("props").contains_key("field"),
            present,
            "{attr}"
        );
    }

    /// A field naming a renamed type by its Rust ident resolves to the
    /// renamed component, as in the OpenAPI document (it used to be `{}`).
    #[test]
    fn renamed_field_type_resolves_through_its_rust_ident() {
        let mut user = StructMetadata::new(
            "UserDto".to_string(),
            r#"#[schema(name = "UserDto")] pub struct User { pub id: i32 }"#.to_string(),
        );
        user.rust_ident = Some("User".to_string());
        let storage = HashMap::from([("UserDto".to_string(), user)]);
        let item = parse("pub struct Post { pub author: User }");
        let schema = build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &storage);
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(
            json["properties"]["author"]["$ref"], "#/components/schemas/UserDto",
            "{json}"
        );
    }

    #[test]
    fn option_field_is_required_and_nullable() {
        let item = parse(r"pub struct WithOpt { pub field: Option<String> }");
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        // serde always writes it (as `null` when `None`).
        let required = schema
            .required
            .as_ref()
            .is_some_and(|r| r.iter().any(|f| f == "field"));
        assert!(required, "an Option<T> field is always written");
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(
            json["properties"]["field"]["type"],
            serde_json::json!(["string", "null"])
        );
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
        let output = generate_filtered_schema(
            &item,
            &HashSet::new(),
            &HashSet::new(),
            &empty_storage(),
            &BTreeMap::new(),
            &[],
        )
        .to_string();
        assert!(
            output.contains("from_compiled_json"),
            "schema! must emit a from_compiled_json reconstruction, got: {output}"
        );
        // The serialized spec carries the property + required set.
        assert!(output.contains("properties"), "spec must carry properties");
        assert!(output.contains("required"), "spec must carry required");
    }

    /// `schema!` carries the same `default`s as the component schema: the
    /// derive's pre-extracted fn default, a type default, an explicit
    /// `#[schema(default)]` — and none for an omitted field.
    #[test]
    fn generate_applies_field_defaults() {
        let item = parse(
            r#"pub struct S {
                #[serde(default = "limit")] pub limit: u32,
                #[serde(default)] pub bio: String,
                #[schema(default = "7")] pub page: u32,
                #[serde(default = "gone")] pub gone: u32,
            }"#,
        );
        let defaults = BTreeMap::from([
            ("limit".to_string(), serde_json::json!(10)),
            ("gone".to_string(), serde_json::json!(1)),
        ]);
        let omit = HashSet::from(["gone".to_string()]);
        let output = generate_filtered_schema(
            &item,
            &omit,
            &HashSet::new(),
            &empty_storage(),
            &defaults,
            &[],
        );
        let syn::Expr::Call(call) = syn::parse2::<syn::Expr>(output).unwrap() else {
            panic!("a call");
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(json),
            ..
        }) = &call.args[0]
        else {
            panic!("a string literal");
        };
        let spec: serde_json::Value = serde_json::from_str(&json.value()).unwrap();
        let defaults: Vec<_> = ["limit", "bio", "page", "gone"]
            .iter()
            .map(|field| spec["properties"][field]["default"].clone())
            .collect();
        assert_eq!(
            defaults,
            [
                serde_json::json!(10),
                serde_json::json!(""),
                serde_json::json!(7),
                serde_json::Value::Null
            ]
        );
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
    // R25-SM1-L3: a `skip_serializing`-only field is not in the serialize-side
    // schema either, so picking it is refused like a skipped field.
    #[case::skip_serializing_pick(
        r#"S, pick = ["secret"]"#,
        Some(
            "field `secret` does not exist in the schema of `S`. Available fields: [\"user_name\"]"
        )
    )]
    fn validates_filter_names(#[case] input: &str, #[case] expected: Option<&str>) {
        let item = parse(
            r#"#[serde(rename_all = "camelCase")] pub struct S { pub user_name: String, #[serde(skip)] pub hidden: i32, #[serde(skip_serializing)] pub secret: String }"#,
        );
        let input: SchemaInput = syn::parse_str(input).unwrap();
        let result = validate_filter_names(&item, &input, "S");
        assert_eq!(result.err().map(|e| e.to_string()).as_deref(), expected);
    }

    /// SM1-M1 (round 38): `omit` / `pick` name a raw identifier by its unraw
    /// spelling as parsed, so `r#type` removes (or keeps) the `type` field
    /// rather than matching nothing (an omit that did nothing; a pick that
    /// dropped every field).
    #[rstest::rstest]
    #[case::omit_raw(r#"S, omit = ["r#type"]"#, &["other", "userName"])]
    #[case::pick_raw(r#"S, pick = ["r#type"]"#, &["type"])]
    fn raw_identifier_filter_names_select_their_field(
        #[case] input: &str,
        #[case] expected: &[&str],
    ) {
        let item = parse(
            r#"#[serde(rename_all = "camelCase")] pub struct S { pub user_name: String, pub r#type: i32, pub other: i32 }"#,
        );
        let input = pick_input(input);
        assert!(validate_filter_names(&item, &input, "S").is_ok());
        let omit: HashSet<String> = input.omit.unwrap_or_default().into_iter().collect();
        let pick: HashSet<String> = input.pick.unwrap_or_default().into_iter().collect();
        let schema = build_filtered_schema(&item, &omit, &pick, &empty_storage());
        assert_eq!(
            schema.properties.unwrap().keys().collect::<Vec<_>>(),
            expected
        );
    }

    fn kept_fields(filtered: &syn::ItemStruct) -> Vec<String> {
        filtered
            .fields
            .iter()
            .map(|f| f.ident.as_ref().unwrap().to_string())
            .collect()
    }

    /// R26-SM1-L1: `b` is the Rust identifier of one field and the JSON name
    /// of another. `pick` / `omit` select the identifier's field only.
    #[test]
    fn filter_name_selects_the_rust_identifier_not_another_fields_json_name() {
        let item = parse(r#"pub struct S { #[serde(rename = "b")] pub a: i32, pub b: i32 }"#);
        let pick = HashSet::from(["b".to_string()]);
        let filtered = filter_struct_fields(&item, &HashSet::new(), &pick).unwrap();
        assert_eq!(kept_fields(&filtered), ["b"]);
        let omit = HashSet::from(["b".to_string()]);
        let filtered = filter_struct_fields(&item, &omit, &HashSet::new()).unwrap();
        assert_eq!(kept_fields(&filtered), ["a"]);
    }

    /// A `skip`ped field is not in the schema, so its identifier cannot take
    /// a name from an emitted field's JSON name.
    #[test]
    fn filter_name_ignores_skipped_fields() {
        let item = parse(
            r#"pub struct S { #[serde(skip)] pub b: i32, #[serde(rename = "b")] pub a: i32 }"#,
        );
        assert!(validate_filter_names(&item, &pick_input(r#"S, pick = ["b"]"#), "S").is_ok());
        let pick = HashSet::from(["b".to_string()]);
        let filtered = filter_struct_fields(&item, &HashSet::new(), &pick).unwrap();
        assert_eq!(kept_fields(&filtered), ["a"]);
    }

    /// `cfg` twins share one Rust identifier, so a name still selects both
    /// (the `cfg` decides which one the schema carries).
    #[test]
    fn filter_name_selects_both_cfg_twins() {
        let item = parse(
            "pub struct S { #[cfg(feature = \"x\")] pub limit: u32, #[cfg(not(feature = \"x\"))] pub limit: u64, pub other: u8 }",
        );
        assert!(validate_filter_names(&item, &pick_input(r#"S, pick = ["limit"]"#), "S").is_ok());
        let pick = HashSet::from(["limit".to_string()]);
        let filtered = filter_struct_fields(&item, &HashSet::new(), &pick).unwrap();
        assert_eq!(kept_fields(&filtered), ["limit", "limit"]);
    }

    /// R26-SM1-L1: a JSON name shared by fields with different identifiers
    /// would select all of them; it is refused at validation with a spanned
    /// error instead.
    #[test]
    fn validate_filter_names_refuses_an_ambiguous_json_name() {
        let item = parse(
            r#"pub struct S { #[serde(rename = "x")] pub a: i32, #[serde(rename = "x")] pub c: i32 }"#,
        );
        let err = validate_filter_names(&item, &pick_input(r#"S, pick = ["x"]"#), "S").unwrap_err();
        assert_eq!(
            err.to_string(),
            "`x` is the JSON name of more than one field of `S` ([\"a\", \"c\"]); name the field by its Rust identifier"
        );
    }

    /// M1 (round 27): a JSON name shared by a `cfg` twin pair is not ambiguous
    /// when only one twin exists in the build. Here the `feature = "a"` twin is
    /// the one that stays, so `"v"` names exactly one field.
    #[rstest::rstest]
    #[case::feature_on(r#"S, omit = ["v"]"#)]
    #[case::feature_on_rust_name(r#"S, omit = ["x"]"#)]
    fn feature_gated_json_name_twin_is_not_ambiguous(#[case] input: &str) {
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--crate-name\napp\n--cfg\nfeature=\"a\"",
        );
        let item = parse(
            r#"struct S {
                #[cfg(feature = "a")] #[serde(rename = "v")] x: i32,
                #[cfg(not(feature = "a"))] #[serde(rename = "v")] y: i32,
                other: i32,
            }"#,
        );
        assert!(validate_filter_names(&item, &pick_input(input), "S").is_ok());
    }

    /// R44-SM1-L2: a `serde(skip)` / `skip_serializing` a statically active
    /// `cfg_attr` adds leaves the field out of the serialize side, so a `pick`
    /// of it is refused like a plain skip. An inactive or undecided predicate
    /// (and a non-serde item) keeps the field a candidate.
    #[rstest::rstest]
    #[case::active_skip("#[cfg_attr(all(), serde(skip))]", true)]
    #[case::active_skip_serializing("#[cfg_attr(all(), serde(skip_serializing))]", true)]
    #[case::inactive_skip("#[cfg_attr(any(), serde(skip))]", false)]
    #[case::undecided_skip("#[cfg_attr(some_unknown_flag, serde(skip))]", false)]
    #[case::other_serde_item("#[cfg_attr(all(), serde(rename = \"y\"))]", false)]
    #[case::non_serde_item("#[cfg_attr(all(), derive(Debug))]", false)]
    #[case::malformed_cfg_attr("#[cfg_attr(all())]", false)]
    #[case::bare_serde_path("#[cfg_attr(all(), serde)]", false)]
    #[case::non_cfg_attr("#[doc = \"x\"]", false)]
    fn cfg_attr_serde_skip_refuses_a_pick(#[case] attr: &str, #[case] refused: bool) {
        let item = parse(&format!(
            "pub struct S {{ pub user_name: String, {attr} pub gated: i32 }}"
        ));
        let result = validate_filter_names(&item, &pick_input(r#"S, pick = ["gated"]"#), "S");
        assert_eq!(result.is_err(), refused, "{result:?}");
    }

    fn pick_input(input: &str) -> SchemaInput {
        syn::parse_str(input).unwrap()
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
