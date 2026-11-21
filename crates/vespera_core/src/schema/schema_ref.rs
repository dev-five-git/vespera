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
/// reference; a `$ref` with no sibling keyword (`x-*` extensions do not
/// count) is a reference, while one with a sibling keyword (`nullable`,
/// `description`, …) is an inline [`Schema`], so those
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
                // Dropped like an unknown key, but it leaves `pure_ref` alone:
                // `{"$ref": .., "x-foo": 1}` still loads as a reference.
                SchemaField::Extension => {
                    let _ = access.next_value::<IgnoredAny>()?;
                }
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
        // A `$ref` (a sibling `$ref`, or one in `allOf` / `oneOf` / `not`) is
        // not resolvable here, so whether it admits `null` is unknown.  Such a
        // `type` array is re-expressed as the equivalent `anyOf: [{type: T},
        // {type: null}]` (`type_array_as_any_of`): the `$ref` stays ANDed with
        // `T` or `null`, exactly as the loaded document says.  Narrowing it to
        // `type: T` could drop a `null` the document accepts.
        let type_kept = self.type_nullable
            && (schema.ref_path.is_some()
                || (schema.composition_mentions_ref() && !schema.composition_admits_null()));
        if type_kept {
            type_array_as_any_of(schema);
        }
        // Otherwise the type array's `null` is kept only when no sibling
        // keyword rejects it (`{type:[T,"null"], enum:[a,b]}` or
        // `{type:[T,"null"], allOf:[{type:T}]}` reject `null`; a `oneOf` /
        // `anyOf` with a `{type:null}` branch admits it).  Mapping the rejecting
        // cases to `nullable` would widen them on re-serialization (`null`
        // appended to the enum / an `anyOf [.., {type:null}]` wrapper), so the
        // `null` is dropped there instead, the same accepted set.
        // An explicit 3.0 `nullable` keeps its widening meaning on purpose:
        // 3.0.3 narrowed it (no effect without an explicit `type`, and an
        // `enum` without `null` still rejects `null`), but 3.0.0-3.0.2 and
        // common generators read `{type, nullable: true, enum: [..]}` as a
        // nullable enum, which is also what `Schema::nullable` means for
        // vespera's own `Option<T>`.  Narrowing would make such documents
        // reject `null` they were written to accept.
        let type_admits_null = self.type_nullable
            && !type_kept
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
    PropertyNames,
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
    /// An `x-*` specification extension: dropped on load, and (unlike a
    /// keyword) it does not turn a `$ref` object into an inline schema.
    Extension,
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
            "propertyNames" => SchemaField::PropertyNames,
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
            _ if value.starts_with("x-") => SchemaField::Extension,
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
        SchemaField::PropertyNames => schema.property_names = Some(access.next_value()?),
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
        SchemaField::Unknown | SchemaField::Extension => {
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

/// Prefix of a component schema `$ref` (`#/components/schemas/<name>`).
const SCHEMA_REF_PREFIX: &str = "#/components/schemas/";

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Encode a component name as the segment that follows
/// `#/components/schemas/` in a `$ref`.
///
/// The name is a JSON Pointer token (`~` becomes `~0`, `/` becomes `~1`)
/// and every byte outside the RFC 3986 unreserved set is then
/// percent-encoded, so a name such as `Pet/Dog` or `My Dto` yields a
/// `$ref` that resolves to the component stored under that name.
#[must_use]
pub fn escape_schema_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'~' => out.push_str("~0"),
            b'/' => out.push_str("~1"),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => {
                out.push(char::from(byte));
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
                out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
            }
        }
    }
    out
}

/// Inverse of [`escape_schema_name`]: percent-decode, then undo the JSON
/// Pointer escaping. A `%` not followed by two hex digits, or a `~` that is
/// not part of `~0` / `~1`, is kept as written.
#[must_use]
pub fn unescape_schema_name(segment: &str) -> String {
    let percent_decoded = percent_decode(segment.as_bytes());
    let mut out = Vec::with_capacity(percent_decoded.len());
    let mut rest = percent_decoded.as_slice();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = match (byte, tail.first()) {
            (b'~', Some(b'1')) => {
                out.push(b'/');
                &tail[1..]
            }
            (b'~', Some(b'0')) => {
                out.push(b'~');
                &tail[1..]
            }
            _ => {
                out.push(byte);
                tail
            }
        };
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-decode `bytes`; a `%` not followed by two hex digits stays as is.
fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&byte, tail)) = rest.split_first() {
        rest = if let (b'%', Some(value)) = (byte, hex_pair(tail)) {
            out.push(value);
            &tail[2..]
        } else {
            out.push(byte);
            tail
        };
    }
    out
}

/// The byte two leading ASCII hex digits of `bytes` encode.
fn hex_pair(bytes: &[u8]) -> Option<u8> {
    match bytes {
        [high, low, ..] => Some((hex_value(*high)? << 4) | hex_value(*low)?),
        _ => None,
    }
}

const fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// The component name a `$ref` points at, when `ref_path` is a
/// `#/components/schemas/` reference (decoded with [`unescape_schema_name`]).
/// A JSON Pointer suffix after the name (`/properties/b`) is not part of the
/// name: the escaped name never holds a raw `/`, so the first `/` ends it.
#[must_use]
pub fn schema_name_of_ref(ref_path: &str) -> Option<String> {
    let segment = ref_path.strip_prefix(SCHEMA_REF_PREFIX)?;
    let name = segment.split_once('/').map_or(segment, |(name, _)| name);
    Some(unescape_schema_name(name))
}

impl Reference {
    /// Create a new reference
    #[must_use]
    pub const fn new(ref_path: String) -> Self {
        Self { ref_path }
    }

    /// Create a component schema reference. The name is escaped with
    /// [`escape_schema_name`], so it always resolves to the component of
    /// that exact name.
    #[must_use]
    pub fn schema(name: &str) -> Self {
        let escaped = escape_schema_name(name);
        let mut ref_path = String::with_capacity(SCHEMA_REF_PREFIX.len() + escaped.len());
        ref_path.push_str(SCHEMA_REF_PREFIX);
        ref_path.push_str(&escaped);
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
///
/// Deserialization is hand-written (a boolean stays [`Self::Bool`], an
/// object goes through the [`SchemaRef`] visitor) rather than untagged, so
/// an invalid nested schema reports its own error instead of serde's
/// generic "did not match any variant".
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    /// `additionalProperties: true | false`.
    Bool(bool),
    /// `additionalProperties: <schema | $ref>`.
    Schema(SchemaRef),
}

impl<'de> Deserialize<'de> for AdditionalProperties {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(AdditionalPropertiesVisitor)
    }
}

/// Re-expresses the `type: T` of a loaded `type: [T, "null"]` as the
/// equivalent `anyOf: [{type: T}, {type: null}]`, for a schema whose `$ref`
/// keeps the `null` from being provable.  With no `anyOf` yet the choice is
/// set on the schema itself; an existing `anyOf` is left alone and the choice
/// goes into an `allOf` member instead (both are ANDed with the schema).
/// `type` is always `Some` here: a `type` array naming `null` with no other
/// type collapses to `type: "null"` before this runs.
fn type_array_as_any_of(schema: &mut Schema) {
    let typed = Schema {
        schema_type: schema.schema_type.take(),
        ..Schema::default()
    };
    let null = Schema {
        schema_type: Some(SchemaType::Null),
        ..Schema::default()
    };
    let choice = vec![
        SchemaRef::Inline(Box::new(typed)),
        SchemaRef::Inline(Box::new(null)),
    ];
    if schema.any_of.is_none() {
        schema.any_of = Some(choice);
    } else {
        schema
            .all_of
            .get_or_insert_with(Vec::new)
            .push(SchemaRef::Inline(Box::new(Schema {
                any_of: Some(choice),
                ..Schema::default()
            })));
    }
}

struct AdditionalPropertiesVisitor;

impl<'de> serde::de::Visitor<'de> for AdditionalPropertiesVisitor {
    type Value = AdditionalProperties;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a boolean or an OpenAPI schema reference or inline schema object")
    }

    fn visit_bool<E: DeError>(self, value: bool) -> Result<Self::Value, E> {
        Ok(AdditionalProperties::Bool(value))
    }

    fn visit_map<M: MapAccess<'de>>(self, access: M) -> Result<Self::Value, M::Error> {
        SchemaRefVisitor
            .visit_map(access)
            .map(AdditionalProperties::Schema)
    }
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
    fn additional_properties_keeps_booleans_and_nested_schema_errors() {
        let parse = serde_json::from_str::<AdditionalProperties>;
        assert!(matches!(
            parse("true").unwrap(),
            AdditionalProperties::Bool(true)
        ));
        assert!(matches!(
            parse("false").unwrap(),
            AdditionalProperties::Bool(false)
        ));
        assert!(matches!(
            parse(r##"{"$ref":"#/components/schemas/A"}"##).unwrap(),
            AdditionalProperties::Schema(SchemaRef::Ref(_))
        ));
        let error = parse(r#"{"type":["string","integer"]}"#)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("not representable"),
            "unexpected error: {error}"
        );
        let error = parse("[]").unwrap_err().to_string();
        assert!(
            error.contains("a boolean or an OpenAPI schema"),
            "unexpected error: {error}"
        );

        let error = serde_json::from_str::<Schema>(
            r#"{"type":"object","additionalProperties":{"type":["string","integer"]}}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("not representable"),
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
    fn extension_siblings_keep_a_reference_pure() {
        for json in [
            r##"{"$ref":"#/components/schemas/A","x-foo":1}"##,
            r##"{"x-foo":{"nested":true},"$ref":"#/components/schemas/A"}"##,
        ] {
            match serde_json::from_str::<SchemaRef>(json).unwrap() {
                SchemaRef::Ref(reference) => {
                    assert_eq!(reference.ref_path, "#/components/schemas/A");
                }
                SchemaRef::Inline(_) => panic!("an x-* sibling must not make {json} inline"),
            }
        }
    }

    #[test]
    fn keyword_siblings_still_make_a_reference_inline() {
        for json in [
            r##"{"$ref":"#/components/schemas/A","nullable":true}"##,
            r##"{"$ref":"#/components/schemas/A","unknownKeyword":1}"##,
            r##"{"$ref":"#/components/schemas/A","x-foo":1,"description":"d"}"##,
        ] {
            match serde_json::from_str::<SchemaRef>(json).unwrap() {
                SchemaRef::Inline(schema) => {
                    assert_eq!(schema.ref_path.as_deref(), Some("#/components/schemas/A"));
                }
                SchemaRef::Ref(_) => panic!("a keyword sibling must keep {json} inline"),
            }
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

#[cfg(test)]
mod escape_tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::plain("User", "User")]
    #[case::dotted_and_dashed("Tree_i32.Input-v2", "Tree_i32.Input-v2")]
    #[case::slash("Pet/Dog", "Pet~1Dog")]
    #[case::tilde("A~B", "A~0B")]
    #[case::tilde_then_one("a~1b", "a~01b")]
    #[case::space("My Dto", "My%20Dto")]
    #[case::percent("50%", "50%25")]
    #[case::non_ascii("사용자", "%EC%82%AC%EC%9A%A9%EC%9E%90")]
    fn escape_applies_pointer_then_percent_encoding(#[case] name: &str, #[case] escaped: &str) {
        assert_eq!(escape_schema_name(name), escaped);
        assert_eq!(unescape_schema_name(escaped), name);
    }

    #[rstest]
    #[case::hex_lower("a%2fb", "a/b")]
    #[case::hex_upper("a%2Fb", "a/b")]
    fn unescape_decodes_percent_hex_in_either_case(#[case] segment: &str, #[case] name: &str) {
        assert_eq!(unescape_schema_name(segment), name);
    }

    #[rstest]
    #[case::bad_hex("100%zz", "100%zz")]
    #[case::sign_is_not_hex("%+1", "%+1")]
    #[case::truncated_percent("end%2", "end%2")]
    #[case::lone_tilde("x~y", "x~y")]
    #[case::trailing_tilde("end~", "end~")]
    #[case::invalid_utf8_is_lossy("%FF", "\u{FFFD}")]
    fn unescape_keeps_input_that_is_not_an_escape(#[case] segment: &str, #[case] name: &str) {
        assert_eq!(unescape_schema_name(segment), name);
    }

    #[test]
    fn schema_reference_escapes_the_component_name() {
        assert_eq!(
            Reference::schema("Pet/Dog").ref_path,
            "#/components/schemas/Pet~1Dog"
        );
        assert_eq!(
            Reference::schema("My Dto").ref_path,
            "#/components/schemas/My%20Dto"
        );
        assert_eq!(
            Reference::schema("User").ref_path,
            "#/components/schemas/User"
        );
    }

    #[test]
    fn schema_name_of_ref_round_trips_the_escaped_name() {
        let reference = Reference::schema("Pet/Dog~v2");
        assert_eq!(
            schema_name_of_ref(&reference.ref_path).as_deref(),
            Some("Pet/Dog~v2")
        );
        assert_eq!(schema_name_of_ref("#/components/pathItems/Pet"), None);
    }

    #[rstest]
    #[case::plain_pointer("#/components/schemas/A/properties/b", Some("A"))]
    #[case::escaped_slash_pointer("#/components/schemas/Pet~1Dog/items", Some("Pet/Dog"))]
    #[case::bare_name("#/components/schemas/A", Some("A"))]
    #[case::not_a_schema("#/components/parameters/A/x", None)]
    fn schema_name_of_ref_drops_the_pointer_suffix(
        #[case] ref_path: &str,
        #[case] name: Option<&str>,
    ) {
        assert_eq!(schema_name_of_ref(ref_path).as_deref(), name);
    }
}
