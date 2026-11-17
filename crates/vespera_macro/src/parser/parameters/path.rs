use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::{Schema, SchemaRef},
};

use super::shared::{
    MAX_CONVERSION_DEPTH, deserialized_instead, input_rename_all, named_struct_fields,
    serde_field_name, struct_definition,
};
use crate::{
    parser::schema::{
        SerdeSide, apply_field_rules, extract_aliases, extract_doc_comment, extract_field_codec,
        extract_flatten, extract_serde_skip_flags, parse_type_to_schema_ref,
        schema_attrs::extract_schema_constraints, wire_override::field_schema_ref,
    },
    schema_macro::type_utils::{is_map_type as utils_is_map_type, last_path_segment, option_inner},
};

/// A required `in: path` parameter.
pub(in crate::parser) fn path_parameter(name: &str, schema: SchemaRef) -> Parameter {
    Parameter {
        name: name.to_string(),
        r#in: ParameterLocation::Path,
        description: None,
        required: Some(true),
        schema: Some(schema),
        example: None,
        style: None,
        explode: None,
        ..Default::default()
    }
}

/// Inline `{type: string}` — the schema of a path segment whose Rust type is
/// unknown (no extractor, a missing tuple position, or a struct field that
/// does not exist).
pub(in crate::parser) fn string_path_schema() -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema::string()))
}

/// One `in: path` parameter per `{name}` of the route path, typed from the
/// handler's `Path<T>` extractor:
///
/// - `Path<(A, B)>`: positional — the n-th tuple element types the n-th
///   segment (missing positions fall back to `string`); a longer tuple is
///   aligned from the end (its leading elements belong to a nest prefix).
/// - `Path<Struct>` (a known named-field struct): each segment is typed by the
///   struct field with the same serde name (`rename` / `rename_all`
///   respected); unmatched segments fall back to `string`.
/// - `Path<HashMap<String, V>>` / `Path<Vec<T>>`: every segment is a `V` / `T`.
/// - a struct with container `#[serde(from = "X")]` / `try_from`: typed from `X`.
/// - a newtype struct `struct Ids(X);` with several segments: typed from `X`.
/// - any other `Path<T>`: `T` types the segment when the route has exactly
///   one; with several segments a scalar cannot match them, so each is
///   documented as `string`.
pub(in crate::parser) fn path_extractor_parameters(
    inner_ty: &Type,
    path_params: &[String],
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Vec<Parameter> {
    if let Type::Tuple(tuple) = inner_ty {
        // axum fills the tuple in full-path order, so a tuple LONGER than the
        // route's own parameters starts with the ones a nest prefix
        // (`Router::nest("/{tenant}", ..)`) supplies: the route's segments are
        // its LAST elements.
        let offset = tuple.elems.len().saturating_sub(path_params.len());
        return path_params
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let schema =
                    tuple
                        .elems
                        .get(offset + idx)
                        .map_or_else(string_path_schema, |elem| {
                            parse_type_to_schema_ref(
                                segment_type(elem),
                                known_schemas,
                                struct_definitions,
                            )
                        });
                path_parameter(name, schema)
            })
            .collect();
    }

    path_parameters_from(inner_ty, path_params, known_schemas, struct_definitions, 0)
}

fn path_parameters_from(
    inner_ty: &Type,
    path_params: &[String],
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
    depth: usize,
) -> Vec<Parameter> {
    if let Type::Tuple(_) = inner_ty {
        return path_extractor_parameters(inner_ty, path_params, known_schemas, struct_definitions);
    }

    // `Path<HashMap<String, V>>` / `Path<Vec<T>>`: axum hands every segment
    // to the map / sequence, so each one is a `V` / `T` — never the whole
    // object or array.
    if let Some(element) = path_collection_element(inner_ty) {
        return path_params
            .iter()
            .map(|name| {
                path_parameter(
                    name,
                    parse_type_to_schema_ref(
                        segment_type(element),
                        known_schemas,
                        struct_definitions,
                    ),
                )
            })
            .collect();
    }

    let struct_item = struct_definition(inner_ty, struct_definitions);
    // Container `#[serde(from = "X")]` / `try_from` (or `#[schema(value_type
    // = X)]`), or `#[serde(transparent)]` (named or tuple struct): the
    // segments are deserialized as `X` / the transparent field's type, not
    // as these fields. A plain newtype (`struct Ids(TwoIds);`,
    // `struct Segs(Vec<u32>);`) is read as its field too: axum's
    // `deserialize_newtype_struct` hands it the whole path, so several
    // segments are typed by the field's type (one segment keeps the
    // newtype's own schema below).
    let multi = path_params.len() > 1;
    if depth < MAX_CONVERSION_DEPTH
        && let Some(source) = struct_item.as_ref().and_then(|item| {
            deserialized_instead(item).or_else(|| multi.then(|| newtype_source(item)).flatten())
        })
    {
        return path_parameters_from(
            &source,
            path_params,
            known_schemas,
            struct_definitions,
            depth + 1,
        );
    }
    if let Some(struct_item) = struct_item
        .as_ref()
        .filter(|struct_item| matches!(struct_item.fields, syn::Fields::Named(_)))
    {
        return path_params
            .iter()
            .map(|name| {
                let found = find_path_field(struct_item, name, struct_definitions, depth);
                let field = found.as_ref().map(|(field, _)| field);
                let schema =
                    found
                        .as_ref()
                        .map_or_else(string_path_schema, |(field, flattened)| {
                            let schema =
                                typed_field_schema(field, known_schemas, struct_definitions);
                            // serde buffers a flattened struct's values through
                            // `deserialize_any`, which axum's path deserializer
                            // answers with a string: only a string field reads it
                            // back (a number / bool field rejects it, or an `Option`
                            // flatten silently becomes `None`).
                            if *flattened && !is_string_schema(&schema) {
                                string_path_schema()
                            } else {
                                schema
                            }
                        });
                // The field's doc comment documents the parameter.
                Parameter {
                    description: field.and_then(|field| extract_doc_comment(&field.attrs)),
                    ..path_parameter(name, schema)
                }
            })
            .collect();
    }

    // `Path<Ids>` with `struct Ids(u32, Uuid)`: axum fills a tuple struct
    // positionally, like a tuple (only the positions serde reads count, and a
    // longer struct is aligned from the end).
    if let Some(syn::Fields::Unnamed(fields)) = struct_item.as_ref().map(|item| &item.fields) {
        let read: Vec<&syn::Field> = fields
            .unnamed
            .iter()
            .filter(|field| {
                let flags = extract_serde_skip_flags(&field.attrs);
                !(flags.skip || flags.skip_deserializing)
            })
            .collect();
        // Every tuple struct but a syntactic newtype (serde reads that as its
        // field): one read position next to skipped ones is still a
        // one-element `deserialize_tuple_struct`.
        if fields.unnamed.len() > 1 && !read.is_empty() {
            let offset = read.len().saturating_sub(path_params.len());
            return path_params
                .iter()
                .enumerate()
                .map(|(idx, name)| {
                    let field = read.get(offset + idx);
                    let schema = field.map_or_else(string_path_schema, |field| {
                        typed_field_schema(field, known_schemas, struct_definitions)
                    });
                    Parameter {
                        description: field.and_then(|field| extract_doc_comment(&field.attrs)),
                        ..path_parameter(name, schema)
                    }
                })
                .collect();
        }
    }

    if let [name] = path_params {
        return vec![path_parameter(
            name,
            parse_type_to_schema_ref(segment_type(inner_ty), known_schemas, struct_definitions),
        )];
    }
    path_params
        .iter()
        .map(|name| path_parameter(name, string_path_schema()))
        .collect()
}

/// The type a syntactic newtype struct (`struct Ids(TwoIds);`) is read as:
/// its field's type, or the field's `#[schema(value_type = T)]`. `None` for
/// any other struct, a skipped field, and a field whose wire shape is not
/// known (`#[schema(any)]`, a custom `deserialize_with`).
fn newtype_source(struct_item: &syn::ItemStruct) -> Option<Type> {
    let syn::Fields::Unnamed(fields) = &struct_item.fields else {
        return None;
    };
    let [field] = fields.unnamed.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let flags = extract_serde_skip_flags(&field.attrs);
    let constraints = extract_schema_constraints(&field.attrs);
    if flags.skip
        || flags.skip_deserializing
        || constraints.is_any()
        || extract_field_codec(&field.attrs).custom_for(SerdeSide::Deserialize)
    {
        return None;
    }
    Some(
        constraints
            .value_type
            .map_or_else(|| field.ty.clone(), |value_type| value_type.0),
    )
}

/// The type a path segment is read as. serde hands a path segment to an
/// `Option<T>` as `Some` (`visit_some`), never as `null`, so the `Option`
/// wrapper is dropped: the segment stays a required `T`.
fn segment_type(ty: &Type) -> &Type {
    option_inner(ty).unwrap_or(ty)
}

/// The schema of a path struct field as serde reads it, with the
/// `#[schema(..)]` / `#[garde(..)]` rules a validated path enforces.
fn typed_field_schema(
    field: &syn::Field,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> SchemaRef {
    let mut constraints = extract_schema_constraints(&field.attrs);
    // A path segment is never `null` (see [`segment_type`]), also when the
    // field's type or its `#[schema(value_type = ..)]` is an `Option`.
    let mut field = field.clone();
    field.ty = segment_type(&field.ty).clone();
    if let Some(value_type) = constraints.value_type.as_mut() {
        value_type.0 = segment_type(&value_type.0).clone();
    }
    let mut schema = field_schema_ref(
        &field,
        &constraints,
        SerdeSide::Deserialize,
        known_schemas,
        struct_definitions,
    );
    apply_field_rules(&mut schema, &field, &constraints);
    schema
}

/// The field serde fills from the path segment `name`: one read under that
/// deserialize-side name or `#[serde(alias)]`, searched through the structs
/// of `#[serde(flatten)]` fields too (then flagged `true`); never a skipped
/// field.
fn find_path_field(
    struct_item: &syn::ItemStruct,
    name: &str,
    struct_definitions: &HashMap<&str, &str>,
    depth: usize,
) -> Option<(syn::Field, bool)> {
    let rename_all = input_rename_all(&struct_item.attrs);
    for field in &struct_item.fields {
        let flags = extract_serde_skip_flags(&field.attrs);
        if flags.skip || flags.skip_deserializing {
            continue;
        }
        if extract_flatten(&field.attrs) {
            let flattened = option_inner(&field.ty).unwrap_or(&field.ty);
            if depth < MAX_CONVERSION_DEPTH
                && let Some(inner) = named_struct_fields(flattened, struct_definitions)
                && let Some((found, _)) =
                    find_path_field(&inner, name, struct_definitions, depth + 1)
            {
                return Some((found, true));
            }
            continue;
        }
        if serde_field_name(field, rename_all.as_deref()) == name
            || extract_aliases(&field.attrs)
                .iter()
                .any(|alias| alias == name)
        {
            return Some((field.clone(), false));
        }
    }
    None
}

/// An inline `{type: string, ..}` schema (a string field, `Uuid`, ...).
fn is_string_schema(schema: &SchemaRef) -> bool {
    matches!(schema, SchemaRef::Inline(schema) if schema.schema_type == Some(vespera_core::schema::SchemaType::String))
}

/// The per-segment type of a `Path<Map<K, V>>` (`V`), `Path<Seq<(K, V)>>`
/// (`V`) or `Path<Seq<T>>` (`T`).
fn path_collection_element(ty: &Type) -> Option<&Type> {
    let segment = last_path_segment(ty)?;
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut types = args.args.iter().filter_map(|arg| match arg {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    });
    if utils_is_map_type(ty) {
        types.nth(1)
    } else if ["Vec", "VecDeque", "HashSet", "BTreeSet"]
        .iter()
        .any(|name| segment.ident == name)
    {
        // A sequence of `(key, value)` pairs (`Path<Vec<(String, V)>>`) gets
        // one pair per segment: the segment itself is the `V`.
        types.next().map(|element| match element {
            Type::Tuple(pair) if pair.elems.len() == 2 => &pair.elems[1],
            other => other,
        })
    } else {
        None
    }
}

pub(super) fn parse_path_extractor(
    ty: &Type,
    path_params: &[String],
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Path" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    let parameters =
        path_extractor_parameters(inner_ty, path_params, known_schemas, struct_definitions);
    (!parameters.is_empty()).then_some(parameters)
}

pub(super) fn parse_bare_path_parameter(
    param_name: &str,
    ty: &Type,
    path_param_set: &HashSet<&str>,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    if !path_param_set.contains(param_name) {
        return None;
    }
    let schema = parse_type_to_schema_ref(segment_type(ty), known_schemas, struct_definitions);
    // A type with no known shape (a custom extractor such as `org: OrgCtx`)
    // says nothing about the segment: keep its `{type: string}` placeholder
    // rather than widening it to any value (`{}`).
    if matches!(&schema, SchemaRef::Inline(inline) if serde_json::to_string(inline).is_ok_and(|json| json == "{}"))
    {
        return None;
    }
    Some(vec![path_parameter(param_name, schema)])
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use vespera_core::route::ParameterLocation;

    use super::parse_path_extractor;
    use crate::parser::parameters::parse_function_parameter;

    /// Lifetime / const arguments are skipped when picking the element type.
    #[rstest::rstest]
    #[case::map_with_lifetime("HashMap<'a, String, u64>", Some("u64"))]
    #[case::vec_with_lifetime("Vec<'a, u32>", Some("u32"))]
    #[case::map_without_value("HashMap<'a, String>", None)]
    #[case::plain("u64", None)]
    #[case::other_generic("Option<u64>", None)]
    fn collection_element_skips_non_type_arguments(
        #[case] ty: &str,
        #[case] expected: Option<&str>,
    ) {
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        let element = super::path_collection_element(&ty).map(|t| quote::quote!(#t).to_string());
        assert_eq!(element.as_deref(), expected);
    }

    #[test]
    fn path_param_by_name_match() {
        let func: syn::ItemFn = syn::parse_str("fn test(user_id: i32) {}").unwrap();
        let path_params = vec!["user_id".to_string()];
        let path_param_set: HashSet<&str> = path_params.iter().map(String::as_str).collect();

        for arg in &func.sig.inputs {
            let result = parse_function_parameter(
                arg,
                &path_params,
                &path_param_set,
                &HashSet::new(),
                &HashMap::new(),
            );
            assert!(result.is_some(), "Expected path parameter by name match");
            let params = result.unwrap();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].r#in, ParameterLocation::Path);
            assert_eq!(params[0].name, "user_id");
        }
    }

    #[test]
    fn tuple_extractor_shorter_than_path_falls_back_to_string() {
        let ty: syn::Type = syn::parse_quote!((u32,));
        let names = vec!["id".to_string(), "rest".to_string()];
        let params =
            super::path_extractor_parameters(&ty, &names, &HashSet::new(), &HashMap::new());
        let types: Vec<_> = params
            .iter()
            .map(|p| match &p.schema {
                Some(vespera_core::schema::SchemaRef::Inline(s)) => {
                    (p.name.as_str(), s.schema_type)
                }
                other => panic!("unexpected schema {other:?}"),
            })
            .collect();
        assert_eq!(
            types,
            vec![
                ("id", Some(vespera_core::schema::SchemaType::Integer)),
                ("rest", Some(vespera_core::schema::SchemaType::String)),
            ]
        );
    }

    #[test]
    fn path_extractor_rejects_missing_or_non_type_generic() {
        let bare: syn::Type = syn::parse_quote!(Path);
        assert!(parse_path_extractor(&bare, &[], &HashSet::new(), &HashMap::new()).is_none());
        let lifetime: syn::Type = syn::parse_quote!(Path<'static>);
        assert!(parse_path_extractor(&lifetime, &[], &HashSet::new(), &HashMap::new()).is_none());
    }

    /// `#[schema(..)]` / `#[garde(..)]` rules on a `Path<Struct>` field are
    /// documented on the path parameter's schema.
    #[test]
    fn path_struct_parameters_document_validation_rules() {
        let ty: syn::Type = syn::parse_quote!(Ids);
        let definitions: HashMap<&str, &str> = std::iter::once((
            "Ids",
            r#"pub struct Ids { #[garde(prefix("u-"))] pub user: String, #[schema(minimum = 1)] pub post: u64 }"#,
        ))
        .collect();
        let names = vec!["user".to_string(), "post".to_string()];
        let params = super::path_extractor_parameters(&ty, &names, &HashSet::new(), &definitions);
        insta::assert_snapshot!(
            "path_struct_parameters_validation_rules",
            serde_json::to_string_pretty(&params).unwrap()
        );
    }

    /// serde reads a path segment through `visit_some` (never `null`): an
    /// `Option` segment is a required, non-nullable parameter of its inner
    /// type, in a struct field (also `value_type`), a tuple element, a
    /// collection element and a bare argument.
    #[test]
    fn option_path_segments_are_not_nullable() {
        let defs = HashMap::from([(
            "Ids",
            "pub struct Ids { pub id: Option<u64>, #[schema(value_type = Option<u32>)] pub org: String }",
        )]);
        let names = vec!["id".to_string(), "org".to_string()];
        let ty: syn::Type = syn::parse_quote!(Ids);
        let mut documented = super::path_extractor_parameters(&ty, &names, &HashSet::new(), &defs);
        let tuple: syn::Type = syn::parse_quote!((Option<u64>, Option<u32>));
        documented.extend(super::path_extractor_parameters(
            &tuple,
            &names,
            &HashSet::new(),
            &HashMap::new(),
        ));
        let collection: syn::Type = syn::parse_quote!(Vec<Option<u32>>);
        documented.extend(super::path_extractor_parameters(
            &collection,
            &names,
            &HashSet::new(),
            &HashMap::new(),
        ));
        let bare: syn::Type = syn::parse_quote!(Option<u64>);
        let path_param_set: HashSet<&str> = ["id"].into_iter().collect();
        documented.extend(
            super::parse_bare_path_parameter(
                "id",
                &bare,
                &path_param_set,
                &HashSet::new(),
                &HashMap::new(),
            )
            .unwrap_or_default(),
        );
        assert_eq!(documented.len(), 7);
        for param in &documented {
            assert_eq!(param.required, Some(true), "{}", param.name);
            let json = serde_json::to_string(&param.schema).unwrap();
            assert!(!json.contains("null"), "{}: {json}", param.name);
            assert!(json.contains("\"integer\""), "{}: {json}", param.name);
        }
    }

    #[test]
    fn path_struct_fields_honour_value_type_and_custom_deserializers() {
        let ty: syn::Type = syn::parse_quote!(Key);
        let defs = HashMap::from([(
            "Key",
            r#"pub struct Key {
                #[serde(deserialize_with = "slug")]
                #[schema(value_type = String)]
                pub org: OrgSlug,
                #[serde(with = "hex")]
                pub id: Vec<u8>,
            }"#,
        )]);
        let names = vec!["org".to_string(), "id".to_string()];
        let params = super::path_extractor_parameters(&ty, &names, &HashSet::new(), &defs);
        let schemas: Vec<String> = params
            .iter()
            .map(|p| serde_json::to_string(&p.schema).unwrap())
            .collect();
        insta::assert_snapshot!("path_wire_override_fields", schemas.join("\n"));
    }
}
