//! `Option<T>` → a schema that also accepts `null`.
//!
//! The `nullable` flag only reaches the wire through `type: [T, "null"]` (a
//! typed schema) or the nullable-`$ref` `anyOf` (see `vespera_core`'s
//! `Schema` serializer). A schema WITHOUT a `type` — a generic enum inlined as
//! `oneOf`, a flattening generic struct inlined as `allOf` — would silently
//! drop it, so such a schema is wrapped in `anyOf: [<schema>, {type: null}]`
//! instead.

use vespera_core::schema::{Schema, SchemaRef, SchemaType};

/// `schema_ref` made nullable (idempotent: `Option<Option<T>>` and an already
/// nullable schema never get a second `null` branch).
pub fn nullable(schema_ref: SchemaRef) -> SchemaRef {
    let mut schema = match schema_ref {
        SchemaRef::Ref(reference) => {
            return SchemaRef::Inline(Box::new(Schema::nullable_reference(reference.ref_path)));
        }
        SchemaRef::Inline(schema) => schema,
    };
    if schema.ref_path.is_none() && composition_rejects_null(&schema) {
        return SchemaRef::Inline(Box::new(Schema {
            any_of: Some(vec![
                SchemaRef::Inline(schema),
                SchemaRef::Inline(Box::new(Schema::new(SchemaType::Null))),
            ]),
            ..Schema::default()
        }));
    }
    // `type: [T, "null"]` next to an `enum` still rejects `null` unless the
    // value list names it.
    if let Some(values) = &mut schema.r#enum
        && !values.contains(&serde_json::Value::Null)
    {
        values.push(serde_json::Value::Null);
    }
    schema.nullable = Some(true);
    SchemaRef::Inline(schema)
}

/// Whether a composition keyword (`oneOf` / `allOf` / `not`, or an `anyOf`
/// without a `null` branch) makes `schema` reject `null` even when it is
/// flagged `nullable`.
fn composition_rejects_null(schema: &Schema) -> bool {
    schema.one_of.is_some()
        || schema.all_of.is_some()
        || schema.not.is_some()
        || schema
            .any_of
            .as_ref()
            .is_some_and(|members| !members.iter().any(is_null_schema))
}

fn is_null_schema(member: &SchemaRef) -> bool {
    matches!(member, SchemaRef::Inline(schema) if schema.schema_type == Some(SchemaType::Null))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use rstest::rstest;

    use super::super::parse_type_to_schema_ref;

    fn render(ty_src: &str) -> String {
        let known: HashSet<String> = ["A", "B", "Either", "Page", "Meta", "Status"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let definitions: HashMap<String, String> = [
            ("Either", "enum Either<L, R> { Left(L), Right(R) }"),
            (
                "Page",
                "struct Page<T> { #[serde(flatten)] item: T, total: u32 }",
            ),
            ("Meta", "struct Meta { at: String }"),
            ("A", "struct A { a: i32 }"),
            ("B", "struct B { b: i32 }"),
            (
                "Status",
                "enum Status<T> { Active, Gone, #[serde(skip)] Never(T) }",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let ty: syn::Type = syn::parse_str(ty_src).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &definitions);
        format!(
            "{ty_src} => {}",
            serde_json::to_string(&schema_ref).unwrap()
        )
    }

    /// A type-less inline schema keeps its `null` branch; nested / already
    /// nullable options never get a second one.
    #[rstest]
    #[case::generic_enum_one_of("option_generic_enum_one_of", "Option<Either<A, B>>")]
    #[case::generic_flatten_all_of("option_generic_flatten_all_of", "Option<Page<Meta>>")]
    #[case::nested_option_one_of("option_option_generic_enum", "Option<Option<Either<A, B>>>")]
    #[case::nested_option_ref("option_option_ref", "Option<Option<A>>")]
    #[case::nested_option_scalar("option_option_scalar", "Option<Option<i32>>")]
    #[case::generic_unit_enum("option_generic_string_enum", "Option<Status<A>>")]
    #[case::value_stays_any("option_value", "Option<serde_json::Value>")]
    fn option_keeps_null(#[case] name: &str, #[case] ty_src: &str) {
        insta::assert_snapshot!(name, render(ty_src));
    }
}
