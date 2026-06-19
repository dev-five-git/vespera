//! Schema-related structure definitions

use serde::{Deserialize, Serialize};
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
        use serde::de::Error as _;
        // OpenAPI is always JSON; buffer the node so a *pure* reference
        // can be distinguished from a `$ref` carrying sibling keywords.
        let value = serde_json::Value::deserialize(deserializer)?;
        // Pure reference: an object whose ONLY key is `$ref` with a string
        // value.  A `$ref` with any sibling (`nullable`, `description`, …)
        // is an inline schema, so the siblings are preserved instead of
        // being dropped by the prior untagged `Ref`-first match.
        if let serde_json::Value::Object(map) = &value
            && map.len() == 1
            && let Some(serde_json::Value::String(ref_path)) = map.get("$ref")
        {
            return Ok(Self::Ref(Reference::new(ref_path.clone())));
        }
        serde_json::from_value::<Schema>(value)
            .map(|schema| Self::Inline(Box::new(schema)))
            .map_err(D::Error::custom)
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// Schema reference (`$ref`).
    ///
    /// A *pure* reference should be expressed as [`SchemaRef::Ref`].
    /// This field exists only for the one legitimate mixed form OpenAPI
    /// 3.1 permits — a **nullable reference** (`$ref` + `nullable`) —
    /// which is best built through [`Schema::nullable_reference`] rather
    /// than by hand, to avoid accidentally mixing `$ref` with unrelated
    /// inline constraints (the invalid state flagged by CORE-03).
    #[serde(rename = "$ref")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_path: Option<String>,
    /// Schema type
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<SchemaType>,
    /// Format (for numbers or strings)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Title
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Example
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,
    /// Examples
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<Vec<serde_json::Value>>,

    // Number constraints
    /// Minimum value
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_number_constraint"
    )]
    pub minimum: Option<f64>,
    /// Maximum value
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_number_constraint"
    )]
    pub maximum: Option<f64>,
    /// Exclusive minimum boundary (OpenAPI 3.1 / JSON Schema 2020-12 numeric form).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_number_constraint"
    )]
    pub exclusive_minimum: Option<f64>,
    /// Exclusive maximum boundary (OpenAPI 3.1 / JSON Schema 2020-12 numeric form).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_number_constraint"
    )]
    pub exclusive_maximum: Option<f64>,
    /// Multiple of
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_number_constraint"
    )]
    pub multiple_of: Option<f64>,

    // String constraints
    /// Minimum length
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// Maximum length
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    /// Pattern (regex)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    // Array constraints
    /// Array item schema.
    ///
    /// No outer `Box`: [`SchemaRef::Inline`] already boxes the nested
    /// [`Schema`], so the recursive type is finite without a second
    /// indirection (CORE-02).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<SchemaRef>,
    /// Prefix items for tuple arrays (`OpenAPI` 3.1 / JSON Schema 2020-12)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_items: Option<Vec<SchemaRef>>,
    /// Minimum number of items
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_items: Option<usize>,
    /// Maximum number of items
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    /// Unique items flag
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique_items: Option<bool>,

    // Object constraints
    /// Property definitions
    #[serde(skip_serializing_if = "is_empty_properties")]
    pub properties: Option<BTreeMap<String, SchemaRef>>,
    /// List of required properties
    #[serde(skip_serializing_if = "is_empty_required")]
    pub required: Option<Vec<String>>,
    /// `additionalProperties`: a boolean or a value-schema (CORE-04).
    ///
    /// Typed as [`AdditionalProperties`] (untagged) instead of a raw
    /// `serde_json::Value`, so invalid shapes can't be constructed and
    /// the value-schema case avoids the `SchemaRef -> serde_json::Value`
    /// round-trip the parser previously paid.  Wire output is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<AdditionalProperties>,
    /// Minimum number of properties
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_properties: Option<usize>,
    /// Maximum number of properties
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_properties: Option<usize>,

    // General constraints
    /// Enum values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<serde_json::Value>>,
    /// All conditions must be satisfied (AND)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_of: Option<Vec<SchemaRef>>,
    /// At least one condition must be satisfied (OR)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<SchemaRef>>,
    /// Exactly one condition must be satisfied (XOR)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<SchemaRef>>,
    /// Condition must not be satisfied (NOT).
    ///
    /// No outer `Box` — [`SchemaRef::Inline`] already boxes the nested
    /// schema (CORE-02).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not: Option<SchemaRef>,

    /// Discriminator for polymorphic schemas (used with oneOf/anyOf/allOf)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<Discriminator>,

    /// Nullable flag
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    /// Read-only flag
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    /// Write-only flag
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_only: Option<bool>,
    /// External documentation reference
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,

    // JSON Schema 2020-12 dynamic references
    /// Definitions ($defs) - reusable schema definitions
    #[serde(rename = "$defs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defs: Option<BTreeMap<String, Self>>,
    /// Dynamic anchor ($dynamicAnchor) - defines a dynamic anchor
    #[serde(rename = "$dynamicAnchor")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_anchor: Option<String>,
    /// Dynamic reference ($dynamicRef) - references a dynamic anchor
    #[serde(rename = "$dynamicRef")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_ref: Option<String>,
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

    /// Build a **nullable reference** schema — `{ "$ref": <path>,
    /// "nullable": true }`.
    ///
    /// This is the single legitimate mixed `$ref` form (CORE-03): a
    /// reference that is also allowed to be `null`.  Centralizing it
    /// here keeps `ref_path` from being hand-mixed with unrelated inline
    /// constraints at call sites.  `ref_path` is the full reference
    /// path (e.g. `"#/components/schemas/User"`); `schema_type` stays
    /// `None` so only `$ref` + `nullable` are emitted.
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
                Self {
                    description: Some(format!(
                        "vespera: schema unavailable — macro/serde drift ({e})"
                    )),
                    ..Self::default()
                }
            }
        }
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
}

#[cfg(test)]
mod tests;
