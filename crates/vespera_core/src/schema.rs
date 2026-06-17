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
    /// Exclusive minimum
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_minimum: Option<bool>,
    /// Exclusive maximum
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_maximum: Option<bool>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, SchemaRef>>,
    /// List of required properties
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// generated user code.
    #[must_use]
    pub fn from_compiled_json(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
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
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(Schema::string(), SchemaType::String)]
    #[case(Schema::integer(), SchemaType::Integer)]
    #[case(Schema::number(), SchemaType::Number)]
    #[case(Schema::boolean(), SchemaType::Boolean)]
    fn primitive_helpers_set_schema_type(#[case] schema: Schema, #[case] expected: SchemaType) {
        assert_eq!(schema.schema_type, Some(expected));
    }

    #[test]
    fn array_helper_sets_type_and_items() {
        let item_schema = Schema::boolean();
        let schema = Schema::array(SchemaRef::Inline(Box::new(item_schema.clone())));

        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        let items = schema.items.expect("items should be set");
        match items {
            SchemaRef::Inline(inner) => {
                assert_eq!(inner.schema_type, Some(SchemaType::Boolean));
            }
            SchemaRef::Ref(_) => panic!("array helper should set inline items"),
        }
    }

    #[test]
    fn object_helper_initializes_collections() {
        let schema = Schema::object();

        assert_eq!(schema.schema_type, Some(SchemaType::Object));
        let props = schema.properties.expect("properties should be initialized");
        assert!(props.is_empty());
        let required = schema.required.expect("required should be initialized");
        assert!(required.is_empty());
    }

    #[test]
    fn serialize_number_constraint_none_serializes_null() {
        // Direct call bypasses skip_serializing_if to cover the None branch
        let result =
            super::serialize_number_constraint(&None, serde_json::value::Serializer).unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }

    #[test]
    fn serialize_minimum_whole_number_as_integer() {
        let schema = Schema {
            minimum: Some(0.0),
            ..Schema::integer()
        };
        let json = serde_json::to_string(&schema).unwrap();
        // Must be "minimum":0 (integer), NOT "minimum":0.0
        assert!(
            json.contains("\"minimum\":0"),
            "expected integer 0, got: {json}"
        );
        assert!(
            !json.contains("\"minimum\":0.0"),
            "must not contain 0.0: {json}"
        );
    }

    #[test]
    fn serialize_minimum_fractional_as_float() {
        let schema = Schema {
            minimum: Some(1.5),
            ..Schema::number()
        };
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            json.contains("\"minimum\":1.5"),
            "expected 1.5, got: {json}"
        );
    }

    #[test]
    fn serialize_minimum_none_omitted() {
        let schema = Schema::integer();
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            !json.contains("minimum"),
            "None minimum should be omitted: {json}"
        );
    }

    #[test]
    fn serialize_maximum_whole_number_as_integer() {
        let schema = Schema {
            maximum: Some(100.0),
            ..Schema::integer()
        };
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            json.contains("\"maximum\":100"),
            "expected integer 100, got: {json}"
        );
        assert!(
            !json.contains("\"maximum\":100.0"),
            "must not contain 100.0: {json}"
        );
    }

    #[test]
    fn serialize_out_of_i64_range_constraint_stays_float() {
        // A whole-number constraint beyond i64 range must NOT saturate to
        // i64::MAX — it stays a float so the spec keeps the real value.
        let schema = Schema {
            maximum: Some(1e20),
            ..Schema::number()
        };
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            !json.contains(&i64::MAX.to_string()),
            "must not saturate to i64::MAX: {json}"
        );
        // Parse back: the constraint value must be preserved exactly,
        // regardless of serde's float formatting.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["maximum"].as_f64(),
            Some(1e20),
            "constraint value must be preserved: {json}"
        );
    }

    #[test]
    fn serialize_multiple_of_whole_number_as_integer() {
        let schema = Schema {
            multiple_of: Some(2.0),
            ..Schema::integer()
        };
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            json.contains("\"multipleOf\":2"),
            "expected integer 2, got: {json}"
        );
        assert!(
            !json.contains("\"multipleOf\":2.0"),
            "must not contain 2.0: {json}"
        );
    }

    // ── CORE-04: typed `additionalProperties` (untagged) ─────────────
    //
    // The untagged enum MUST serialize to the bare JSON Schema wire form
    // (a `true`/`false` or the schema object/`$ref`) — byte-identical to
    // the previous `serde_json::Value` representation — and round-trip
    // back to the right variant.  Untagged deserialization is
    // order-sensitive, so these lock the contract.

    #[test]
    fn additional_properties_bool_serializes_bare() {
        let schema = Schema {
            additional_properties: Some(AdditionalProperties::Bool(false)),
            ..Schema::object()
        };
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            json.contains("\"additionalProperties\":false"),
            "bool must serialize as a bare boolean, got: {json}"
        );
    }

    #[test]
    fn additional_properties_schema_ref_serializes_as_ref() {
        let schema = Schema {
            additional_properties: Some(AdditionalProperties::Schema(SchemaRef::Ref(
                Reference::schema("User"),
            ))),
            ..Schema::object()
        };
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            json.contains("\"additionalProperties\":{\"$ref\":\"#/components/schemas/User\"}"),
            "schema-ref must serialize as a bare $ref object, got: {json}"
        );
    }

    #[test]
    fn additional_properties_roundtrips_each_variant() {
        // bool → Bool
        let v: AdditionalProperties = serde_json::from_str("true").unwrap();
        assert!(matches!(v, AdditionalProperties::Bool(true)));
        // {"$ref":...} → Schema(Ref)
        let v: AdditionalProperties =
            serde_json::from_str(r##"{"$ref":"#/components/schemas/X"}"##).unwrap();
        assert!(matches!(v, AdditionalProperties::Schema(SchemaRef::Ref(_))));
        // inline schema object → Schema(Inline)
        let v: AdditionalProperties = serde_json::from_str(r#"{"type":"string"}"#).unwrap();
        assert!(matches!(
            v,
            AdditionalProperties::Schema(SchemaRef::Inline(_))
        ));
    }

    // ── CORE-03: nullable-reference constructor ──────────────────────

    #[test]
    fn nullable_reference_emits_ref_plus_nullable_only() {
        let schema = Schema::nullable_reference("#/components/schemas/User".to_owned());
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            json.contains("\"$ref\":\"#/components/schemas/User\""),
            "must carry the $ref: {json}"
        );
        assert!(
            json.contains("\"nullable\":true"),
            "must be nullable: {json}"
        );
        // schema_type stays None so no stray `"type"` is emitted alongside.
        assert!(
            !json.contains("\"type\":"),
            "a nullable reference must not also emit a type: {json}"
        );
    }

    // ── SchemaRef: $ref-sibling preservation ─────────────────────────
    //
    // The prior `#[serde(untagged)]` `Ref`-first enum greedily matched
    // ANY object with a `$ref` key and silently dropped its siblings
    // (e.g. a nullable reference's `"nullable": true`).  The custom
    // `Deserialize` treats only a *pure* `{"$ref": <string>}` as a
    // reference; a `$ref` with any sibling becomes an inline `Schema`
    // so the siblings round-trip intact.

    #[test]
    fn schema_ref_pure_ref_deserializes_as_ref() {
        let v: SchemaRef =
            serde_json::from_str(r##"{"$ref":"#/components/schemas/User"}"##).unwrap();
        match v {
            SchemaRef::Ref(r) => assert_eq!(r.ref_path, "#/components/schemas/User"),
            SchemaRef::Inline(_) => panic!("a pure $ref must deserialize as SchemaRef::Ref"),
        }
    }

    #[test]
    fn schema_ref_with_nullable_sibling_preserves_fields() {
        let v: SchemaRef =
            serde_json::from_str(r##"{"$ref":"#/components/schemas/User","nullable":true}"##)
                .unwrap();
        match v {
            SchemaRef::Inline(schema) => {
                assert_eq!(
                    schema.ref_path.as_deref(),
                    Some("#/components/schemas/User"),
                    "the $ref must survive as an inline ref_path"
                );
                assert_eq!(
                    schema.nullable,
                    Some(true),
                    "the nullable sibling must not be dropped"
                );
            }
            SchemaRef::Ref(_) => panic!("$ref with a sibling must not be matched as a bare Ref"),
        }
    }

    #[test]
    fn schema_ref_inline_object_deserializes_as_inline() {
        let v: SchemaRef = serde_json::from_str(r#"{"type":"string"}"#).unwrap();
        assert!(matches!(v, SchemaRef::Inline(_)));
    }

    #[test]
    fn schema_ref_nullable_reference_roundtrips() {
        // Build → serialize → deserialize must keep BOTH `$ref` and `nullable`.
        let original = Schema::nullable_reference("#/components/schemas/User".to_owned());
        let json = serde_json::to_string(&SchemaRef::Inline(Box::new(original))).unwrap();
        let back: SchemaRef = serde_json::from_str(&json).unwrap();
        match back {
            SchemaRef::Inline(s) => {
                assert_eq!(s.ref_path.as_deref(), Some("#/components/schemas/User"));
                assert_eq!(s.nullable, Some(true));
            }
            SchemaRef::Ref(_) => panic!("a nullable reference must round-trip as inline"),
        }
    }
}
