use super::Schema;
use serde::{
    Deserialize, Serialize,
    de::{Error as DeError, IgnoredAny, MapAccess},
};

/// Schema reference or inline schema.
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
        deserializer.deserialize_map(SchemaRefVisitor)
    }
}

struct SchemaRefVisitor;

impl<'de> serde::de::Visitor<'de> for SchemaRefVisitor {
    type Value = SchemaRef;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an OpenAPI schema reference or inline schema object")
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut schema = Schema::default();
        let mut pure_ref = true;
        let mut has_inline_fields = false;
        let mut ref_path = None;
        let mut type_nullable = None;
        let mut nullable = None;

        while let Some(key) = access.next_key::<SchemaField>()? {
            match key {
                SchemaField::RefPath => {
                    let path = access.next_value::<String>()?;
                    if pure_ref && ref_path.is_none() && !has_inline_fields {
                        ref_path = Some(path);
                    } else {
                        pure_ref = false;
                        has_inline_fields = true;
                        schema.ref_path = Some(path);
                    }
                }
                other => {
                    if let Some(path) = ref_path.take() {
                        schema.ref_path = Some(path);
                    }
                    pure_ref = false;
                    has_inline_fields = true;
                    apply_schema_field(
                        other,
                        &mut schema,
                        &mut type_nullable,
                        &mut nullable,
                        &mut access,
                    )?;
                }
            }
        }

        if pure_ref && let Some(path) = ref_path {
            return Ok(SchemaRef::Ref(Reference::new(path)));
        }
        schema.nullable = match type_nullable {
            Some(true) => Some(true),
            None => nullable,
            Some(false) => nullable.or(Some(false)),
        };
        Ok(SchemaRef::Inline(Box::new(schema)))
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
                    "externalDocs" => SchemaField::ExternalDocs,
                    "$defs" => SchemaField::Defs,
                    "$dynamicAnchor" => SchemaField::DynamicAnchor,
                    "$dynamicRef" => SchemaField::DynamicRef,
                    _ => SchemaField::Unknown,
                })
            }
        }

        deserializer.deserialize_identifier(SchemaFieldVisitor)
    }
}

fn apply_schema_field<'de, M>(
    field: SchemaField,
    schema: &mut Schema,
    type_nullable: &mut Option<bool>,
    nullable: &mut Option<bool>,
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
            *type_nullable = next_nullable;
        }
        SchemaField::Format => schema.format = Some(access.next_value()?),
        SchemaField::Title => schema.title = Some(access.next_value()?),
        SchemaField::Description => schema.description = Some(access.next_value()?),
        SchemaField::Default => schema.default = Some(access.next_value()?),
        SchemaField::Example => schema.example = Some(access.next_value()?),
        SchemaField::Examples => schema.examples = Some(access.next_value()?),
        SchemaField::Minimum => schema.minimum = Some(access.next_value()?),
        SchemaField::Maximum => schema.maximum = Some(access.next_value()?),
        SchemaField::ExclusiveMinimum => schema.exclusive_minimum = Some(access.next_value()?),
        SchemaField::ExclusiveMaximum => schema.exclusive_maximum = Some(access.next_value()?),
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
        SchemaField::Nullable => *nullable = Some(access.next_value()?),
        SchemaField::ReadOnly => schema.read_only = Some(access.next_value()?),
        SchemaField::WriteOnly => schema.write_only = Some(access.next_value()?),
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
