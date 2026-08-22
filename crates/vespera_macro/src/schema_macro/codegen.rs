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
    let known_schemas: HashSet<String> = schema_storage.keys().cloned().collect();
    let struct_definitions: HashMap<String, String> = schema_storage
        .values()
        .map(|s| (s.name.clone(), s.definition.clone()))
        .collect();

    // Single source of truth — identical logic to OpenAPI generation
    // (required-by-nullability, doc descriptions, flatten/transparent,
    // field constraints, `$ref` references).
    let mut schema = parse_struct_to_schema(struct_item, &known_schemas, &struct_definitions);

    // `schema!` layers field filtering on top: keep only the picked /
    // non-omitted properties (matched against BOTH the Rust identifier and
    // the serde-renamed JSON name, as the prior hand-rolled walk did).
    if let Some(keep) = compute_kept_json_names(struct_item, omit_set, pick_set) {
        filter_schema_fields(&mut schema, &keep);
    }

    schema
}

/// Compute the set of serde-renamed JSON field names that survive the
/// `pick`/`omit` filter, or `None` when no filtering is requested (both
/// sets empty → keep every field).
///
/// Mirrors the OpenAPI field walk: `#[serde(skip)]` fields never qualify,
/// and a name matches `omit`/`pick` against EITHER its Rust identifier or
/// its serde-renamed JSON name.
fn compute_kept_json_names(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
) -> Option<HashSet<String>> {
    if omit_set.is_empty() && pick_set.is_empty() {
        return None;
    }
    let rename_all = extract_rename_all(&struct_item.attrs);
    let mut keep = HashSet::new();
    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            if extract_skip(&field.attrs) {
                continue;
            }
            let rust_field_name = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |i| strip_raw_prefix_owned(i.to_string()),
            );
            let field_name = extract_field_rename(&field.attrs)
                .unwrap_or_else(|| rename_field(&rust_field_name, rename_all.as_deref()));
            if !omit_set.is_empty()
                && (omit_set.contains(&rust_field_name) || omit_set.contains(&field_name))
            {
                continue;
            }
            if !pick_set.is_empty()
                && !pick_set.contains(&rust_field_name)
                && !pick_set.contains(&field_name)
            {
                continue;
            }
            keep.insert(field_name);
        }
    }
    Some(keep)
}

/// Retain only `keep` properties (and matching `required` entries) on
/// `schema`, normalizing an emptied `properties`/`required` back to `None`
/// to match [`parse_struct_to_schema`]'s own representation.
fn filter_schema_fields(schema: &mut Schema, keep: &HashSet<String>) {
    if let Some(properties) = &mut schema.properties {
        properties.retain(|name, _| keep.contains(name));
        if properties.is_empty() {
            schema.properties = None;
        }
    }
    if let Some(required) = &mut schema.required {
        required.retain(|name| keep.contains(name));
        if required.is_empty() {
            schema.required = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use crate::metadata::StructMetadata;

    use super::{build_filtered_schema, compute_kept_json_names, generate_filtered_schema};

    fn empty_storage() -> HashMap<String, StructMetadata> {
        HashMap::new()
    }

    fn parse(src: &str) -> syn::ItemStruct {
        syn::parse_str(src).expect("valid struct source")
    }

    /// Regression for the schema!↔OpenAPI drift: a `#[serde(default)]`
    /// non-`Option` field must be REQUIRED (required is nullability-only,
    /// identical to the OpenAPI component schema).  The prior `schema!`
    /// path wrongly excluded defaulted / `skip_serializing_if` fields.
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

    #[test]
    fn skip_serializing_if_field_is_required() {
        let item = parse(
            r#"pub struct WithSkip { #[serde(skip_serializing_if = "Option::is_none")] pub field: String }"#,
        );
        let schema =
            build_filtered_schema(&item, &HashSet::new(), &HashSet::new(), &empty_storage());
        assert!(
            schema
                .required
                .expect("required present")
                .iter()
                .any(|f| f == "field"),
            "skip_serializing_if must not affect required (nullability-only)"
        );
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
    fn filtering_skips_serde_hidden_fields_and_handles_identifierless_field() {
        let mut item = parse("pub struct S { #[serde(skip)] pub hidden: String, pub shown: i32 }");
        let mut pick = HashSet::from(["shown".to_string()]);
        let keep = compute_kept_json_names(&item, &HashSet::new(), &pick)
            .expect("a pick filter always produces a keep set");
        assert_eq!(keep, HashSet::from(["shown".to_string()]));

        let syn::Fields::Named(fields) = &mut item.fields else {
            panic!("fixture is a named struct");
        };
        let shown = fields.named.last_mut().expect("shown field exists");
        shown.ident = None;
        pick = HashSet::from(["unknown".to_string()]);
        let keep = compute_kept_json_names(&item, &HashSet::new(), &pick)
            .expect("a pick filter always produces a keep set");
        assert_eq!(keep, HashSet::from(["unknown".to_string()]));
    }

    #[test]
    fn filtering_every_field_normalizes_empty_properties_and_required() {
        let item = parse("pub struct S { pub only: String }");
        let omit = HashSet::from(["only".to_string()]);
        let schema = build_filtered_schema(&item, &omit, &HashSet::new(), &empty_storage());

        assert!(schema.properties.is_none());
        assert!(schema.required.is_none());
    }
}
