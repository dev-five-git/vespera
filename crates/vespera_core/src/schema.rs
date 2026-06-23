//! Schema-related structure definitions

mod components;
mod schema_ref;
mod serde_impls;

pub use components::{Components, OAuthFlow, OAuthFlows, SecurityScheme, SecuritySchemeType};
pub use schema_ref::{AdditionalProperties, Reference, SchemaRef};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
#[allow(clippy::ref_option)] // serde serialize_with mandates &Option<T> signature
fn serialize_number_constraint<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serde_impls::serialize_number_constraint(value, serializer)
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

    /// Create an object schema without allocating empty `properties` or `required` collections.
    #[must_use]
    pub fn object_empty() -> Self {
        Self::new(SchemaType::Object)
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
            ..Self::object_empty()
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

#[cfg(test)]
mod tests;
