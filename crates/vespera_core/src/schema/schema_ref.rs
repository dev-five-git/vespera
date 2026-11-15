use super::{Schema, SchemaType};
use serde::{
    Deserialize, Serialize,
    de::{Error as DeError, IgnoredAny, MapAccess},
};

/// Schema reference or inline schema.
///
/// A boolean schema (`true` / `false`) deserializes to its equivalent
/// inline object (`{}` / `{"not": {}}`).
///
/// Serializes untagged — a bare `{"$ref": ...}` object for
/// [`SchemaRef::Ref`], the schema object for [`SchemaRef::Inline`].
///
/// Deserialization is a hand-written impl rather than
/// `#[serde(untagged)]`: an untagged `Ref`-first enum greedily matched
/// **any** object carrying a `$ref` key and silently dropped its
/// siblings (e.g. a nullable reference's `"nullable": true`).  The
/// custom impl treats only a *pure* `{"$ref": <string>}` object as a
/// reference; a `$ref` accompanied by any sibling keyword
/// (`nullable`, `description`, …) is an inline [`Schema`], so those
/// siblings survive the round-trip instead of being discarded.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum SchemaRef {
    /// Schema reference (e.g., "#/components/schemas/User")
    Ref(Reference),
    /// Inline schema
    Inline(Box<Schema>),
}

impl<'de> Deserialize<'de> for SchemaRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // `any`, not `map`: a JSON Schema 2020-12 boolean schema is a bare
        // `true` / `false`, which a map-only request would reject.
        deserializer.deserialize_any(SchemaRefVisitor)
    }
}

struct SchemaRefVisitor;

impl<'de> serde::de::Visitor<'de> for SchemaRefVisitor {
    type Value = SchemaRef;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an OpenAPI schema reference or inline schema object")
    }

    /// A boolean schema (OpenAPI 3.1 / JSON Schema 2020-12), stored as its
    /// object equivalent: `true` ≡ `{}` (accepts anything), `false` ≡
    /// `{"not": {}}` (accepts nothing).  It serializes in that object form.
    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        let schema = if value {
            Schema::default()
        } else {
            Schema {
                not: Some(SchemaRef::Inline(Box::default())),
                ..Schema::default()
            }
        };
        Ok(SchemaRef::Inline(Box::new(schema)))
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut schema = Schema::default();
        let mut pure_ref = true;
        let mut has_inline_fields = false;
        let mut ref_path = None;
        let mut pending = PendingFlags::default();

        while let Some(key) = access.next_key::<SchemaField>()? {
            match key {
                // Only the FIRST `$ref`, seen before any inline field, can still
                // make this a pure reference.  A repeated `$ref` (or one that
                // follows an inline field) falls through to the generic arm,
                // which stores it on the inline schema like any other field.
                SchemaField::RefPath if pure_ref && ref_path.is_none() && !has_inline_fields => {
                    ref_path = Some(access.next_value::<String>()?);
                }
                // `_` (not a binding): a binding pattern on its own line gets
                // no coverage region on current rustc.
                _ => {
                    if let Some(path) = ref_path.take() {
                        schema.ref_path = Some(path);
                    }
                    pure_ref = false;
                    has_inline_fields = true;
                    apply_schema_field(key, &mut schema, &mut pending, &mut access)?;
                }
            }
        }

        if pure_ref && let Some(path) = ref_path {
            return Ok(SchemaRef::Ref(Reference::new(path)));
        }
        pending.finish(&mut schema);
        Ok(SchemaRef::Inline(Box::new(schema)))
    }
}

/// Keywords whose final value depends on keys that may come later in the
/// object, resolved once the whole object was read.
#[derive(Default)]
struct PendingFlags {
    /// The `type` array named `"null"`.
    type_nullable: bool,
    /// An explicit (OpenAPI 3.0) `nullable`.
    nullable: Option<bool>,
    exclusive_minimum: Option<ExclusiveBound>,
    exclusive_maximum: Option<ExclusiveBound>,
}

impl PendingFlags {
    fn finish(self, schema: &mut Schema) {
        // A `"null"` in the `type` array admits `null` only when no sibling
        // keyword rejects it: `{type:[T,"null"], enum:[a,b]}` and
        // `{type:[T,"null"], allOf:[..]}` both reject `null`, while a
        // `oneOf` / `anyOf` with a `{type:null}` branch or a `not` of a
        // non-null schema admits it (`Schema::composition_admits_null`).  Mapping those
        // to `nullable` would widen them on re-serialization (`null` appended
        // to the enum / an `anyOf [.., {type:null}]` wrapper), so the type
        // array's `null` is dropped there instead — the same accepted set.
        // A sibling `$ref` is ANDed with the `type` array in 3.1 (like an
        // `allOf` member), so its `null` is dropped there too.
        // An explicit 3.0 `nullable` keeps its widening meaning.
        let type_admits_null = self.type_nullable
            && schema.ref_path.is_none()
            && schema
                .r#enum
                .as_ref()
                .is_none_or(|values| values.contains(&serde_json::Value::Null))
            && schema.composition_admits_null();
        schema.nullable = if type_admits_null {
            Some(true)
        } else {
            self.nullable
        };
        schema.exclusive_minimum =
            ExclusiveBound::resolve(self.exclusive_minimum, &mut schema.minimum);
        schema.exclusive_maximum =
            ExclusiveBound::resolve(self.exclusive_maximum, &mut schema.maximum);
        // An explicit 3.0 `nullable` next to a `$ref` that also carries a
        // `type` or an `anyOf` has no flat serializable form (the nullable
        // `$ref` itself becomes the `anyOf`), so the widening is spelled out
        // here as `anyOf: [<schema>, {type: null}]` — the shape serialization
        // emits for every other nullable composition.
        if schema.nullable == Some(true)
            && schema.ref_path.is_some()
            && (schema.schema_type.is_some() || schema.any_of.is_some())
        {
            schema.nullable = None;
            let inner = std::mem::take(schema);
            let null = Schema {
                schema_type: Some(SchemaType::Null),
                ..Schema::default()
            };
            schema.any_of = Some(vec![
                SchemaRef::Inline(Box::new(inner)),
                SchemaRef::Inline(Box::new(null)),
            ]);
        }
    }
}

/// `exclusiveMinimum` / `exclusiveMaximum`: the OpenAPI 3.1 number, or the
/// OpenAPI 3.0 boolean modifier of `minimum` / `maximum`.
#[derive(Clone, Copy)]
enum ExclusiveBound {
    /// OpenAPI 3.0: `true` makes the sibling `minimum` / `maximum` exclusive.
    Flag(bool),
    /// OpenAPI 3.1: the exclusive bound itself.
    Value(f64),
}

impl ExclusiveBound {
    /// The 3.1 exclusive bound.  A 3.0 `true` moves the inclusive `bound`
    /// (`minimum` / `maximum`) into it; a 3.0 `false` is the default and
    /// leaves `bound` inclusive.
    fn resolve(this: Option<Self>, bound: &mut Option<f64>) -> Option<f64> {
        match this? {
            Self::Value(value) => Some(value),
            Self::Flag(true) => bound.take(),
            Self::Flag(false) => None,
        }
    }
}

impl<'de> Deserialize<'de> for ExclusiveBound {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(ExclusiveBoundVisitor)
    }
}

struct ExclusiveBoundVisitor;

impl serde::de::Visitor<'_> for ExclusiveBoundVisitor {
    type Value = ExclusiveBound;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a number or (OpenAPI 3.0) a boolean")
    }

    fn visit_bool<E: DeError>(self, value: bool) -> Result<Self::Value, E> {
        Ok(ExclusiveBound::Flag(value))
    }

    fn visit_f64<E: DeError>(self, value: f64) -> Result<Self::Value, E> {
        Ok(ExclusiveBound::Value(value))
    }

    #[allow(clippy::cast_precision_loss)] // same conversion serde's own f64 visitor applies
    fn visit_i64<E: DeError>(self, value: i64) -> Result<Self::Value, E> {
        Ok(ExclusiveBound::Value(value as f64))
    }

    #[allow(clippy::cast_precision_loss)] // same conversion serde's own f64 visitor applies
    fn visit_u64<E: DeError>(self, value: u64) -> Result<Self::Value, E> {
        Ok(ExclusiveBound::Value(value as f64))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SchemaField {
    RefPath,
    Type,
    Format,
    Title,
    Description,
    Default,
    Example,
    Examples,
    Minimum,
    Maximum,
    ExclusiveMinimum,
    ExclusiveMaximum,
    MultipleOf,
    MinLength,
    MaxLength,
    Pattern,
    Items,
    PrefixItems,
    MinItems,
    MaxItems,
    UniqueItems,
    Properties,
    Required,
    AdditionalProperties,
    MinProperties,
    MaxProperties,
    Enum,
    AllOf,
    AnyOf,
    OneOf,
    Not,
    Discriminator,
    Nullable,
    ReadOnly,
    WriteOnly,
    Deprecated,
    ExternalDocs,
    Defs,
    DynamicAnchor,
    DynamicRef,
    Unknown,
}

impl<'de> Deserialize<'de> for SchemaField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_identifier(SchemaFieldVisitor)
    }
}

struct SchemaFieldVisitor;

impl serde::de::Visitor<'_> for SchemaFieldVisitor {
    type Value = SchemaField;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON Schema field name")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        Ok(match value {
            "$ref" => SchemaField::RefPath,
            "type" => SchemaField::Type,
            "format" => SchemaField::Format,
            "title" => SchemaField::Title,
            "description" => SchemaField::Description,
            "default" => SchemaField::Default,
            "example" => SchemaField::Example,
            "examples" => SchemaField::Examples,
            "minimum" => SchemaField::Minimum,
            "maximum" => SchemaField::Maximum,
            "exclusiveMinimum" => SchemaField::ExclusiveMinimum,
            "exclusiveMaximum" => SchemaField::ExclusiveMaximum,
            "multipleOf" => SchemaField::MultipleOf,
            "minLength" => SchemaField::MinLength,
            "maxLength" => SchemaField::MaxLength,
            "pattern" => SchemaField::Pattern,
            "items" => SchemaField::Items,
            "prefixItems" => SchemaField::PrefixItems,
            "minItems" => SchemaField::MinItems,
            "maxItems" => SchemaField::MaxItems,
            "uniqueItems" => SchemaField::UniqueItems,
            "properties" => SchemaField::Properties,
            "required" => SchemaField::Required,
            "additionalProperties" => SchemaField::AdditionalProperties,
            "minProperties" => SchemaField::MinProperties,
            "maxProperties" => SchemaField::MaxProperties,
            "enum" => SchemaField::Enum,
            "allOf" => SchemaField::AllOf,
            "anyOf" => SchemaField::AnyOf,
            "oneOf" => SchemaField::OneOf,
            "not" => SchemaField::Not,
            "discriminator" => SchemaField::Discriminator,
            "nullable" => SchemaField::Nullable,
            "readOnly" => SchemaField::ReadOnly,
            "writeOnly" => SchemaField::WriteOnly,
            "deprecated" => SchemaField::Deprecated,
            "externalDocs" => SchemaField::ExternalDocs,
            "$defs" => SchemaField::Defs,
            "$dynamicAnchor" => SchemaField::DynamicAnchor,
            "$dynamicRef" => SchemaField::DynamicRef,
            _ => SchemaField::Unknown,
        })
    }
}

fn apply_schema_field<'de, M>(
    field: SchemaField,
    schema: &mut Schema,
    pending: &mut PendingFlags,
    access: &mut M,
) -> Result<(), M::Error>
where
    M: MapAccess<'de>,
{
    match field {
        SchemaField::RefPath => schema.ref_path = Some(access.next_value()?),
        SchemaField::Type => {
            let (schema_type, next_nullable) = access
                .next_value::<super::serde_impls::SchemaTypeWire>()?
                .into_schema_type_and_nullable::<M::Error>()?;
            schema.schema_type = schema_type;
            pending.type_nullable = next_nullable;
        }
        SchemaField::Format => schema.format = Some(access.next_value()?),
        SchemaField::Title => schema.title = Some(access.next_value()?),
        SchemaField::Description => schema.description = Some(access.next_value()?),
        SchemaField::Default => schema.default = Some(access.next_value()?),
        SchemaField::Example => schema.example = Some(access.next_value()?),
        SchemaField::Examples => schema.examples = Some(access.next_value()?),
        SchemaField::Minimum => schema.minimum = Some(access.next_value()?),
        SchemaField::Maximum => schema.maximum = Some(access.next_value()?),
        SchemaField::ExclusiveMinimum => pending.exclusive_minimum = Some(access.next_value()?),
        SchemaField::ExclusiveMaximum => pending.exclusive_maximum = Some(access.next_value()?),
        SchemaField::MultipleOf => schema.multiple_of = Some(access.next_value()?),
        SchemaField::MinLength => schema.min_length = Some(access.next_value()?),
        SchemaField::MaxLength => schema.max_length = Some(access.next_value()?),
        SchemaField::Pattern => schema.pattern = Some(access.next_value()?),
        SchemaField::Items => schema.items = Some(access.next_value()?),
        SchemaField::PrefixItems => schema.prefix_items = Some(access.next_value()?),
        SchemaField::MinItems => schema.min_items = Some(access.next_value()?),
        SchemaField::MaxItems => schema.max_items = Some(access.next_value()?),
        SchemaField::UniqueItems => schema.unique_items = Some(access.next_value()?),
        SchemaField::Properties => schema.properties = Some(access.next_value()?),
        SchemaField::Required => schema.required = Some(access.next_value()?),
        SchemaField::AdditionalProperties => {
            schema.additional_properties = Some(access.next_value()?);
        }
        SchemaField::MinProperties => schema.min_properties = Some(access.next_value()?),
        SchemaField::MaxProperties => schema.max_properties = Some(access.next_value()?),
        SchemaField::Enum => schema.r#enum = Some(access.next_value()?),
        SchemaField::AllOf => schema.all_of = Some(access.next_value()?),
        SchemaField::AnyOf => schema.any_of = Some(access.next_value()?),
        SchemaField::OneOf => schema.one_of = Some(access.next_value()?),
        SchemaField::Not => schema.not = Some(access.next_value()?),
        SchemaField::Discriminator => schema.discriminator = Some(access.next_value()?),
        SchemaField::Nullable => pending.nullable = Some(access.next_value()?),
        SchemaField::ReadOnly => schema.read_only = Some(access.next_value()?),
        SchemaField::WriteOnly => schema.write_only = Some(access.next_value()?),
        SchemaField::Deprecated => schema.deprecated = Some(access.next_value()?),
        SchemaField::ExternalDocs => schema.external_docs = Some(access.next_value()?),
        SchemaField::Defs => schema.defs = Some(access.next_value()?),
        SchemaField::DynamicAnchor => schema.dynamic_anchor = Some(access.next_value()?),
        SchemaField::DynamicRef => schema.dynamic_ref = Some(access.next_value()?),
        SchemaField::Unknown => {
            let _ = access.next_value::<IgnoredAny>()?;
        }
    }
    Ok(())
}

/// Reference definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// Reference path (e.g., "#/components/schemas/User")
    #[serde(rename = "$ref")]
    pub ref_path: String,
}

impl Reference {
    /// Create a new reference
    #[must_use]
    pub const fn new(ref_path: String) -> Self {
        Self { ref_path }
    }

    /// Create a component schema reference
    #[must_use]
    pub fn schema(name: &str) -> Self {
        // Build with an exact-capacity push instead of `format!` — same
        // string, no formatting machinery and no reallocation.
        const PREFIX: &str = "#/components/schemas/";
        let mut ref_path = String::with_capacity(PREFIX.len() + name.len());
        ref_path.push_str(PREFIX);
        ref_path.push_str(name);
        Self::new(ref_path)
    }
}

/// `additionalProperties` value (JSON Schema / OpenAPI 3.1).
///
/// Either a boolean (`true`/`false` — allow or forbid extra properties)
/// or a schema that every additional property must satisfy.  Untagged,
/// so it serializes to exactly the JSON Schema wire form (a bare
/// `true`/`false` or the schema object / `$ref`) with no wrapper —
/// byte-identical to the previous `serde_json::Value` representation
/// for the values vespera actually emits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    /// `additionalProperties: true | false`.
    Bool(bool),
    /// `additionalProperties: <schema | $ref>`.
    Schema(SchemaRef),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_ref_rejects_non_object_with_descriptive_error() {
        let error = serde_json::from_str::<SchemaRef>("[]").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("an OpenAPI schema reference or inline schema object"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn schema_field_rejects_a_non_string_visitor_input() {
        // `serde_json` rejects a numeric key before the visitor sees it, so
        // drive the visitor directly through a u64 deserializer to reach the
        // `Visitor` impl's inherited (non-`visit_str`) entry points.
        let deserializer = serde::de::value::U64Deserializer::<serde::de::value::Error>::new(42);
        let Err(error) = SchemaField::deserialize(deserializer) else {
            panic!("a u64 schema field must be rejected");
        };

        assert!(
            error.to_string().contains("a JSON Schema field name"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn schema_field_rejects_non_string_identifier_with_descriptive_error() {
        let Err(error) = serde_json::from_str::<SchemaField>("42") else {
            panic!("a numeric schema field must be rejected");
        };

        assert!(
            error.to_string().contains("a JSON Schema field name"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duplicate_ref_keys_produce_inline_schema_and_keep_latest_path() {
        let schema_ref: SchemaRef = serde_json::from_str(
            r##"{"$ref":"#/components/schemas/Old","$ref":"#/components/schemas/New"}"##,
        )
        .unwrap();

        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.ref_path.as_deref(), Some("#/components/schemas/New"));
            }
            SchemaRef::Ref(_) => panic!("duplicate $ref keys must not deserialize as a pure ref"),
        }
    }

    #[test]
    fn unknown_schema_field_is_ignored_without_losing_known_fields() {
        let schema_ref: SchemaRef =
            serde_json::from_str(r#"{"unknown":{"nested":[1,2,3]},"title":"Known title"}"#)
                .unwrap();

        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.title.as_deref(), Some("Known title"));
            }
            SchemaRef::Ref(_) => panic!("an object with inline fields must deserialize inline"),
        }
    }
}
