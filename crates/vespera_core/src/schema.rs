//! Schema-related structure definitions

use serde::{
    Deserialize, Serialize,
    de::{Error as DeError, MapAccess},
    ser::{SerializeSeq, SerializeStruct},
};
use std::collections::BTreeMap;

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
        use serde::de::Error as _;

        let mut ref_path = None;
        let mut inline = serde_json::Map::new();
        while let Some(key) = access.next_key::<String>()? {
            let value = access.next_value::<serde_json::Value>()?;
            if key == "$ref"
                && ref_path.is_none()
                && inline.is_empty()
                && let serde_json::Value::String(path) = value
            {
                ref_path = Some(path);
            } else {
                if let Some(path) = ref_path.take() {
                    inline.insert("$ref".to_owned(), serde_json::Value::String(path));
                }
                inline.insert(key, value);
            }
        }

        if let Some(path) = ref_path {
            return Ok(SchemaRef::Ref(Reference::new(path)));
        }

        serde_json::from_value::<Schema>(serde_json::Value::Object(inline))
            .map(|schema| SchemaRef::Inline(Box::new(schema)))
            .map_err(M::Error::custom)
    }
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

/// JSON Schema type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemaType {
    String,
    Number,
    Integer,
    Boolean,
    Array,
    Object,
    Null,
}

/// Serialize `Option<f64>` as integer when the value has no fractional part.
///
/// Ensures OpenAPI JSON uses `0` instead of `0.0` for integer constraints like
/// `minimum`/`maximum`, matching the convention that integer type bounds are integers.
#[cfg(test)]
#[allow(clippy::ref_option)] // serde serialize_with mandates &Option<T> signature
fn serialize_number_constraint<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(v) if v.fract() == 0.0 => {
            // Float→int casts saturate in Rust, so an out-of-range
            // constraint (e.g. `1e20`) would silently become `i64::MAX`
            // and corrupt the generated spec.  Emit the integer form
            // only when it round-trips exactly back to the original
            // value; otherwise keep the `f64` rendering.
            #[allow(clippy::cast_possible_truncation)]
            let int_val = *v as i64;
            // Exact round-trip check is intentional: we emit the integer
            // form only when `i64 → f64` reproduces the original bits.
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            if int_val as f64 == *v {
                serializer.serialize_some(&int_val)
            } else {
                serializer.serialize_some(v)
            }
        }
        Some(v) => serializer.serialize_some(v),
        None => serializer.serialize_none(),
    }
}

#[allow(clippy::ref_option)] // serde skip_serializing_if mandates &Option<T> signature
fn is_empty_properties(value: &Option<BTreeMap<String, SchemaRef>>) -> bool {
    value.as_ref().is_none_or(BTreeMap::is_empty)
}

#[allow(clippy::ref_option)] // serde skip_serializing_if mandates &Option<T> signature
fn is_empty_required(value: &Option<Vec<String>>) -> bool {
    value.as_ref().is_none_or(Vec::is_empty)
}

/// JSON Schema definition
#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// Schema reference (`$ref`).
    ///
    /// A *pure* reference should be expressed as [`SchemaRef::Ref`].
    /// This field exists only for the one legitimate mixed form OpenAPI
    /// 3.1 permits — a **nullable reference** (`$ref` + `nullable`) —
    /// which is best built through [`Schema::nullable_reference`] rather
    /// than by hand, to avoid accidentally mixing `$ref` with unrelated
    /// inline constraints (the invalid state flagged by CORE-03).
    pub ref_path: Option<String>,
    /// Schema type
    pub schema_type: Option<SchemaType>,
    /// Format (for numbers or strings)
    pub format: Option<String>,
    /// Title
    pub title: Option<String>,
    /// Description
    pub description: Option<String>,
    /// Default value
    pub default: Option<serde_json::Value>,
    /// Example
    pub example: Option<serde_json::Value>,
    /// Examples
    pub examples: Option<Vec<serde_json::Value>>,

    // Number constraints
    /// Minimum value
    pub minimum: Option<f64>,
    /// Maximum value
    pub maximum: Option<f64>,
    /// Exclusive minimum boundary (OpenAPI 3.1 / JSON Schema 2020-12 numeric form).
    pub exclusive_minimum: Option<f64>,
    /// Exclusive maximum boundary (OpenAPI 3.1 / JSON Schema 2020-12 numeric form).
    pub exclusive_maximum: Option<f64>,
    /// Multiple of
    pub multiple_of: Option<f64>,

    // String constraints
    /// Minimum length
    pub min_length: Option<usize>,
    /// Maximum length
    pub max_length: Option<usize>,
    /// Pattern (regex)
    pub pattern: Option<String>,

    // Array constraints
    /// Array item schema.
    ///
    /// No outer `Box`: [`SchemaRef::Inline`] already boxes the nested
    /// [`Schema`], so the recursive type is finite without a second
    /// indirection (CORE-02).
    pub items: Option<SchemaRef>,
    /// Prefix items for tuple arrays (`OpenAPI` 3.1 / JSON Schema 2020-12)
    pub prefix_items: Option<Vec<SchemaRef>>,
    /// Minimum number of items
    pub min_items: Option<usize>,
    /// Maximum number of items
    pub max_items: Option<usize>,
    /// Unique items flag
    pub unique_items: Option<bool>,

    // Object constraints
    /// Property definitions
    pub properties: Option<BTreeMap<String, SchemaRef>>,
    /// List of required properties
    pub required: Option<Vec<String>>,
    /// `additionalProperties`: a boolean or a value-schema (CORE-04).
    ///
    /// Typed as [`AdditionalProperties`] (untagged) instead of a raw
    /// `serde_json::Value`, so invalid shapes can't be constructed and
    /// the value-schema case avoids the `SchemaRef -> serde_json::Value`
    /// round-trip the parser previously paid.  Wire output is unchanged.
    pub additional_properties: Option<AdditionalProperties>,
    /// Minimum number of properties
    pub min_properties: Option<usize>,
    /// Maximum number of properties
    pub max_properties: Option<usize>,

    // General constraints
    /// Enum values
    pub r#enum: Option<Vec<serde_json::Value>>,
    /// All conditions must be satisfied (AND)
    pub all_of: Option<Vec<SchemaRef>>,
    /// At least one condition must be satisfied (OR)
    pub any_of: Option<Vec<SchemaRef>>,
    /// Exactly one condition must be satisfied (XOR)
    pub one_of: Option<Vec<SchemaRef>>,
    /// Condition must not be satisfied (NOT).
    ///
    /// No outer `Box` — [`SchemaRef::Inline`] already boxes the nested
    /// schema (CORE-02).
    pub not: Option<SchemaRef>,

    /// Discriminator for polymorphic schemas (used with oneOf/anyOf/allOf)
    pub discriminator: Option<Discriminator>,

    /// Nullable flag
    pub nullable: Option<bool>,
    /// Read-only flag
    pub read_only: Option<bool>,
    /// Write-only flag
    pub write_only: Option<bool>,
    /// External documentation reference
    pub external_docs: Option<ExternalDocumentation>,

    // JSON Schema 2020-12 dynamic references
    /// Definitions ($defs) - reusable schema definitions
    pub defs: Option<BTreeMap<String, Self>>,
    /// Dynamic anchor ($dynamicAnchor) - defines a dynamic anchor
    pub dynamic_anchor: Option<String>,
    /// Dynamic reference ($dynamicRef) - references a dynamic anchor
    pub dynamic_ref: Option<String>,
}

struct NumberConstraint(f64);

impl Serialize for NumberConstraint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0.fract() == 0.0 {
            #[allow(clippy::cast_possible_truncation)]
            let int_val = self.0 as i64;
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            if int_val as f64 == self.0 {
                return int_val.serialize(serializer);
            }
        }
        self.0.serialize(serializer)
    }
}

struct NullableRefSchema<'a> {
    ref_path: &'a str,
}

impl Serialize for NullableRefSchema<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut out = serializer.serialize_struct("Schema", 1)?;
        out.serialize_field("$ref", self.ref_path)?;
        out.end()
    }
}

struct NullSchema;

impl Serialize for NullSchema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut out = serializer.serialize_struct("Schema", 1)?;
        out.serialize_field("type", &SchemaType::Null)?;
        out.end()
    }
}

struct NullableRefAnyOf<'a> {
    ref_path: &'a str,
}

impl Serialize for NullableRefAnyOf<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&NullableRefSchema {
            ref_path: self.ref_path,
        })?;
        seq.serialize_element(&NullSchema)?;
        seq.end()
    }
}

struct ExamplesWithLegacy<'a> {
    example: Option<&'a serde_json::Value>,
    examples: &'a [serde_json::Value],
}

impl Serialize for ExamplesWithLegacy<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let len = self.examples.len() + usize::from(self.example.is_some());
        let mut seq = serializer.serialize_seq(Some(len))?;
        if let Some(example) = self.example {
            seq.serialize_element(example)?;
        }
        for example in self.examples {
            seq.serialize_element(example)?;
        }
        seq.end()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum SchemaTypeWire {
    Single(SchemaType),
    Multiple(Vec<SchemaType>),
}

impl SchemaTypeWire {
    fn into_schema_type_and_nullable<E>(self) -> Result<(Option<SchemaType>, Option<bool>), E>
    where
        E: DeError,
    {
        match self {
            Self::Single(schema_type) => Ok((Some(schema_type), None)),
            Self::Multiple(schema_types) => {
                let nullable = schema_types.contains(&SchemaType::Null).then_some(true);
                let mut schema_type = None;
                for next_type in schema_types
                    .into_iter()
                    .filter(|schema_type| *schema_type != SchemaType::Null)
                {
                    if let Some(current_type) = schema_type
                        && current_type != next_type
                    {
                        return Err(E::custom(
                            "OpenAPI schema `type` arrays with multiple non-null types are not representable; use anyOf/oneOf instead",
                        ));
                    }
                    schema_type = Some(next_type);
                }
                Ok((schema_type, nullable))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaDeserialize {
    #[serde(rename = "$ref")]
    ref_path: Option<String>,
    #[serde(rename = "type")]
    schema_type: Option<SchemaTypeWire>,
    format: Option<String>,
    title: Option<String>,
    description: Option<String>,
    default: Option<serde_json::Value>,
    example: Option<serde_json::Value>,
    examples: Option<Vec<serde_json::Value>>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    exclusive_minimum: Option<f64>,
    exclusive_maximum: Option<f64>,
    multiple_of: Option<f64>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    pattern: Option<String>,
    items: Option<SchemaRef>,
    prefix_items: Option<Vec<SchemaRef>>,
    min_items: Option<usize>,
    max_items: Option<usize>,
    unique_items: Option<bool>,
    properties: Option<BTreeMap<String, SchemaRef>>,
    required: Option<Vec<String>>,
    additional_properties: Option<AdditionalProperties>,
    min_properties: Option<usize>,
    max_properties: Option<usize>,
    r#enum: Option<Vec<serde_json::Value>>,
    all_of: Option<Vec<SchemaRef>>,
    any_of: Option<Vec<SchemaRef>>,
    one_of: Option<Vec<SchemaRef>>,
    not: Option<SchemaRef>,
    discriminator: Option<Discriminator>,
    nullable: Option<bool>,
    read_only: Option<bool>,
    write_only: Option<bool>,
    external_docs: Option<ExternalDocumentation>,
    #[serde(rename = "$defs")]
    defs: Option<BTreeMap<String, Schema>>,
    #[serde(rename = "$dynamicAnchor")]
    dynamic_anchor: Option<String>,
    #[serde(rename = "$dynamicRef")]
    dynamic_ref: Option<String>,
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SchemaDeserialize::deserialize(deserializer)?;
        let (schema_type, type_nullable) = wire.schema_type.map_or(Ok((None, None)), |wire| {
            wire.into_schema_type_and_nullable::<D::Error>()
        })?;
        let nullable = match type_nullable {
            Some(true) => Some(true),
            None => wire.nullable,
            Some(false) => wire.nullable.or(Some(false)),
        };
        Ok(Self {
            ref_path: wire.ref_path,
            schema_type,
            format: wire.format,
            title: wire.title,
            description: wire.description,
            default: wire.default,
            example: wire.example,
            examples: wire.examples,
            minimum: wire.minimum,
            maximum: wire.maximum,
            exclusive_minimum: wire.exclusive_minimum,
            exclusive_maximum: wire.exclusive_maximum,
            multiple_of: wire.multiple_of,
            min_length: wire.min_length,
            max_length: wire.max_length,
            pattern: wire.pattern,
            items: wire.items,
            prefix_items: wire.prefix_items,
            min_items: wire.min_items,
            max_items: wire.max_items,
            unique_items: wire.unique_items,
            properties: wire.properties,
            required: wire.required,
            additional_properties: wire.additional_properties,
            min_properties: wire.min_properties,
            max_properties: wire.max_properties,
            r#enum: wire.r#enum,
            all_of: wire.all_of,
            any_of: wire.any_of,
            one_of: wire.one_of,
            not: wire.not,
            discriminator: wire.discriminator,
            nullable,
            read_only: wire.read_only,
            write_only: wire.write_only,
            external_docs: wire.external_docs,
            defs: wire.defs,
            dynamic_anchor: wire.dynamic_anchor,
            dynamic_ref: wire.dynamic_ref,
        })
    }
}

impl Serialize for Schema {
    #[allow(clippy::too_many_lines)]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let nullable_ref = self.nullable == Some(true) && self.ref_path.is_some();
        if nullable_ref && self.any_of.is_some() {
            return Err(serde::ser::Error::custom(
                "invalid Schema: nullable `$ref` serializes through anyOf and cannot also carry explicit any_of",
            ));
        }
        let mut out = serializer.serialize_struct("Schema", 42)?;
        if let Some(ref_path) = &self.ref_path {
            if nullable_ref {
                out.serialize_field("anyOf", &NullableRefAnyOf { ref_path })?;
            } else {
                out.serialize_field("$ref", ref_path)?;
            }
        }
        if let Some(schema_type) = self.schema_type {
            let wire = if self.nullable == Some(true) {
                SchemaTypeWire::Multiple(vec![schema_type, SchemaType::Null])
            } else {
                SchemaTypeWire::Single(schema_type)
            };
            out.serialize_field("type", &wire)?;
        }
        if let Some(value) = &self.format {
            out.serialize_field("format", value)?;
        }
        if let Some(value) = &self.title {
            out.serialize_field("title", value)?;
        }
        if let Some(value) = &self.description {
            out.serialize_field("description", value)?;
        }
        if let Some(value) = &self.default {
            out.serialize_field("default", value)?;
        }
        match (&self.example, &self.examples) {
            (Some(example), Some(examples)) => {
                out.serialize_field(
                    "examples",
                    &ExamplesWithLegacy {
                        example: Some(example),
                        examples,
                    },
                )?;
            }
            (Some(example), None) => {
                out.serialize_field(
                    "examples",
                    &ExamplesWithLegacy {
                        example: Some(example),
                        examples: &[],
                    },
                )?;
            }
            (None, Some(examples)) => {
                out.serialize_field("examples", examples)?;
            }
            (None, None) => {}
        }
        if let Some(value) = self.minimum {
            out.serialize_field("minimum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.maximum {
            out.serialize_field("maximum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.exclusive_minimum {
            out.serialize_field("exclusiveMinimum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.exclusive_maximum {
            out.serialize_field("exclusiveMaximum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.multiple_of {
            out.serialize_field("multipleOf", &NumberConstraint(value))?;
        }
        if let Some(value) = self.min_length {
            out.serialize_field("minLength", &value)?;
        }
        if let Some(value) = self.max_length {
            out.serialize_field("maxLength", &value)?;
        }
        if let Some(value) = &self.pattern {
            out.serialize_field("pattern", value)?;
        }
        if let Some(value) = &self.items {
            out.serialize_field("items", value)?;
        }
        if let Some(value) = &self.prefix_items {
            out.serialize_field("prefixItems", value)?;
        }
        if let Some(value) = self.min_items {
            out.serialize_field("minItems", &value)?;
        }
        if let Some(value) = self.max_items {
            out.serialize_field("maxItems", &value)?;
        }
        if let Some(value) = self.unique_items {
            out.serialize_field("uniqueItems", &value)?;
        }
        if !is_empty_properties(&self.properties) {
            out.serialize_field("properties", &self.properties)?;
        }
        if !is_empty_required(&self.required) {
            out.serialize_field("required", &self.required)?;
        }
        if let Some(value) = &self.additional_properties {
            out.serialize_field("additionalProperties", value)?;
        }
        if let Some(value) = self.min_properties {
            out.serialize_field("minProperties", &value)?;
        }
        if let Some(value) = self.max_properties {
            out.serialize_field("maxProperties", &value)?;
        }
        if let Some(value) = &self.r#enum {
            out.serialize_field("enum", value)?;
        }
        if let Some(value) = &self.all_of {
            out.serialize_field("allOf", value)?;
        }
        if let Some(value) = &self.any_of
            && !nullable_ref
        {
            out.serialize_field("anyOf", value)?;
        }
        if let Some(value) = &self.one_of {
            out.serialize_field("oneOf", value)?;
        }
        if let Some(value) = &self.not {
            out.serialize_field("not", value)?;
        }
        if let Some(value) = &self.discriminator {
            out.serialize_field("discriminator", value)?;
        }
        if let Some(value) = self.read_only {
            out.serialize_field("readOnly", &value)?;
        }
        if let Some(value) = self.write_only {
            out.serialize_field("writeOnly", &value)?;
        }
        if let Some(value) = &self.external_docs {
            out.serialize_field("externalDocs", value)?;
        }
        if let Some(value) = &self.defs {
            out.serialize_field("$defs", value)?;
        }
        if let Some(value) = &self.dynamic_anchor {
            out.serialize_field("$dynamicAnchor", value)?;
        }
        if let Some(value) = &self.dynamic_ref {
            out.serialize_field("$dynamicRef", value)?;
        }
        out.end()
    }
}

impl Schema {
    /// Create a new schema of the given type.
    ///
    /// Every other field starts at its [`Default`] (`None`/empty), so a newly
    /// added `Schema` field is auto-defaulted here instead of having to be
    /// appended to a ~40-field manual initializer that drifts out of sync.
    #[must_use]
    pub fn new(schema_type: SchemaType) -> Self {
        Self {
            schema_type: Some(schema_type),
            ..Self::default()
        }
    }

    /// Create a string schema
    #[must_use]
    pub fn string() -> Self {
        Self::new(SchemaType::String)
    }

    /// Create an integer schema
    #[must_use]
    pub fn integer() -> Self {
        Self::new(SchemaType::Integer)
    }

    /// Create a number schema
    #[must_use]
    pub fn number() -> Self {
        Self::new(SchemaType::Number)
    }

    /// Create a boolean schema
    #[must_use]
    pub fn boolean() -> Self {
        Self::new(SchemaType::Boolean)
    }

    /// Create an array schema
    #[must_use]
    pub fn array(items: SchemaRef) -> Self {
        Self {
            items: Some(items),
            ..Self::new(SchemaType::Array)
        }
    }

    /// Create an object schema
    #[must_use]
    pub fn object() -> Self {
        Self {
            properties: Some(BTreeMap::new()),
            required: Some(Vec::new()),
            ..Self::new(SchemaType::Object)
        }
    }

    /// Build a **nullable reference** schema that serializes as OpenAPI 3.1
    /// `anyOf`: `[{ "$ref": <path> }, { "type": "null" }]`.
    ///
    /// This is the single legitimate mixed `$ref` form (CORE-03): a
    /// reference that is also allowed to be `null`.  Centralizing it
    /// here keeps `ref_path` from being hand-mixed with unrelated inline
    /// constraints at call sites.  `ref_path` is the full reference
    /// path (e.g. `"#/components/schemas/User"`); `schema_type` stays
    /// `None` so only the nullable-reference `anyOf` shape is emitted.
    #[must_use]
    pub fn nullable_reference(ref_path: String) -> Self {
        Self {
            ref_path: Some(ref_path),
            schema_type: None,
            nullable: Some(true),
            ..Self::new(SchemaType::Object)
        }
    }

    /// Reconstruct a [`Schema`] from a compile-time-serialized JSON spec.
    ///
    /// This is the bridge the `schema!` proc-macro uses to emit a runtime
    /// `Schema` value that is **identical** to the one the OpenAPI
    /// generator produces for the same type: the macro builds the schema
    /// through the shared `parse_struct_to_schema` path, serializes it to
    /// JSON at compile time, and emits a call to this constructor — so the
    /// `schema!` result can never drift from the documented component
    /// schema (required-by-nullability, doc descriptions,
    /// flatten/transparent, field constraints, `$ref` references).
    ///
    /// The input is always valid JSON (the macro just serialized it via
    /// `serde_json`), so a parse failure is unreachable in practice; it
    /// degrades to [`Schema::default`] rather than panicking inside
    /// generated user code.  A failure would silently drop a component
    /// schema, so it is surfaced via `debug_assert!` (caught in
    /// development / CI) while release builds still degrade gracefully — a
    /// macro/serde drift never goes unnoticed but never panics in
    /// downstream user code either.
    #[must_use]
    pub fn from_compiled_json(json: &str) -> Self {
        match serde_json::from_str(json) {
            Ok(schema) => schema,
            Err(e) => {
                // Surface the (in-practice-unreachable) macro/serde drift in
                // debug / CI builds via `debug_assert!`.  In release, degrade
                // to a VISIBLE sentinel schema (a description-only object)
                // rather than a silent `Schema::default()`, so a drift never
                // disappears unnoticed from the generated spec yet never
                // panics in downstream user code.
                debug_assert!(
                    false,
                    "vespera: Schema::from_compiled_json failed to parse macro-emitted \
                     JSON ({e}); emitting a sentinel schema. This indicates a \
                     vespera bug — the macro serialized a Schema that cannot round-trip."
                );
                schema_parse_failure_sentinel(&e)
            }
        }
    }
}

fn schema_parse_failure_sentinel(error: &serde_json::Error) -> Schema {
    Schema {
        title: Some("VESPERA_SCHEMA_PARSE_ERROR".to_owned()),
        description: Some(format!(
            "vespera: schema unavailable — macro/serde drift ({error})"
        )),
        ..Schema::default()
    }
}

/// External documentation reference
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDocumentation {
    /// Documentation description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Documentation URL
    pub url: String,
}

/// Discriminator object for polymorphism support (`OpenAPI` 3.0/3.1)
///
/// Used with `oneOf`, `anyOf`, `allOf` to aid in serialization, deserialization,
/// and validation when request bodies or response payloads may be one of several types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discriminator {
    /// The name of the property in the payload that will hold the discriminator value
    pub property_name: String,
    /// An object to hold mappings between payload values and schema names or references
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping: Option<BTreeMap<String, String>>,
}

/// `OpenAPI` Components (reusable components)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Components {
    /// Schema definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schemas: Option<BTreeMap<String, Schema>>,
    /// Response definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses: Option<BTreeMap<String, crate::route::Response>>,
    /// Parameter definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<BTreeMap<String, crate::route::Parameter>>,
    /// Example definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<BTreeMap<String, crate::route::Example>>,
    /// Request body definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_bodies: Option<BTreeMap<String, crate::route::RequestBody>>,
    /// Header definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, crate::route::Header>>,
    /// Security scheme definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<BTreeMap<String, SecurityScheme>>,
}

/// Security scheme type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecuritySchemeType {
    ApiKey,
    Http,
    /// OpenAPI's canonical wire name is `mutualTLS` (not the `camelCase`
    /// `mutualTls` the container rule would produce).
    #[serde(rename = "mutualTLS")]
    MutualTls,
    /// OpenAPI's canonical wire name is `oauth2`; the `camelCase` container
    /// rule would otherwise lowercase only the leading char and emit the
    /// invalid `oAuth2`.
    #[serde(rename = "oauth2")]
    OAuth2,
    OpenIdConnect,
}

/// Security scheme definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityScheme {
    /// Security scheme type
    pub r#type: SecuritySchemeType,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Name (for API Key)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Location (for API Key: query, header, cookie)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#in: Option<String>,
    /// Scheme (for HTTP: bearer, basic, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// Bearer format (for HTTP Bearer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_format: Option<String>,
    /// OAuth2 flows (for OAuth2 security schemes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flows: Option<OAuthFlows>,
    /// OpenID Connect discovery URL (for OpenID Connect security schemes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id_connect_url: Option<String>,
}

/// OAuth2 flow definitions for OpenAPI security schemes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlows {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implicit: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<OAuthFlow>,
}

/// OAuth2 flow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    pub scopes: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests;
