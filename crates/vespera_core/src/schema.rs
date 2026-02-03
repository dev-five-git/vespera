//! Schema-related structure definitions

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Schema reference or inline schema
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaRef {
    /// Schema reference (e.g., "#/components/schemas/User")
    Ref(Reference),
    /// Inline schema
    Inline(Box<Schema>),
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
    pub fn new(ref_path: String) -> Self {
        Self { ref_path }
    }

    /// Create a component schema reference
    pub fn schema(name: &str) -> Self {
        Reference::new(format!("#/components/schemas/{}", name))
    }
}

/// JSON Schema type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Number format
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NumberFormat {
    Float,
    Double,
    Int32,
    Int64,
}

/// String format
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StringFormat {
    Date,
    DateTime,
    Password,
    Byte,
    Binary,
    Email,
    Uuid,
    Uri,
    Hostname,
    IpV4,
    IpV6,
}

/// JSON Schema definition
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// Schema reference ($ref) - if present, other fields are ignored
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// Maximum value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// Exclusive minimum
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_minimum: Option<bool>,
    /// Exclusive maximum
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_maximum: Option<bool>,
    /// Multiple of
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// Array item schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<SchemaRef>>,
    /// Prefix items for tuple arrays (OpenAPI 3.1 / JSON Schema 2020-12)
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
    /// Whether additional properties are allowed (can be boolean or SchemaRef)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<serde_json::Value>,
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
    /// Condition must not be satisfied (NOT)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not: Option<Box<SchemaRef>>,

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
    pub defs: Option<BTreeMap<String, Schema>>,
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
    /// Create a new schema
    pub fn new(schema_type: SchemaType) -> Self {
        Self {
            ref_path: None,
            schema_type: Some(schema_type),
            format: None,
            title: None,
            description: None,
            default: None,
            example: None,
            examples: None,
            minimum: None,
            maximum: None,
            exclusive_minimum: None,
            exclusive_maximum: None,
            multiple_of: None,
            min_length: None,
            max_length: None,
            pattern: None,
            items: None,
            prefix_items: None,
            min_items: None,
            max_items: None,
            unique_items: None,
            properties: None,
            required: None,
            additional_properties: None,
            min_properties: None,
            max_properties: None,
            r#enum: None,
            all_of: None,
            any_of: None,
            one_of: None,
            not: None,
            nullable: None,
            read_only: None,
            write_only: None,
            external_docs: None,
            defs: None,
            dynamic_anchor: None,
            dynamic_ref: None,
        }
    }

    /// Create a string schema
    pub fn string() -> Self {
        Self::new(SchemaType::String)
    }

    /// Create an integer schema
    pub fn integer() -> Self {
        Self::new(SchemaType::Integer)
    }

    /// Create a number schema
    pub fn number() -> Self {
        Self::new(SchemaType::Number)
    }

    /// Create a boolean schema
    pub fn boolean() -> Self {
        Self::new(SchemaType::Boolean)
    }

    /// Create an array schema
    pub fn array(items: SchemaRef) -> Self {
        Self {
            items: Some(Box::new(items)),
            ..Self::new(SchemaType::Array)
        }
    }

    /// Create an object schema
    pub fn object() -> Self {
        Self {
            properties: Some(BTreeMap::new()),
            required: Some(Vec::new()),
            ..Self::new(SchemaType::Object)
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

/// OpenAPI Components (reusable components)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Components {
    /// Schema definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schemas: Option<BTreeMap<String, Schema>>,
    /// Response definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses: Option<HashMap<String, crate::route::Response>>,
    /// Parameter definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<HashMap<String, crate::route::Parameter>>,
    /// Example definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<HashMap<String, crate::route::Example>>,
    /// Request body definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_bodies: Option<HashMap<String, crate::route::RequestBody>>,
    /// Header definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, crate::route::Header>>,
    /// Security scheme definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<HashMap<String, SecurityScheme>>,
}

/// Security scheme type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecuritySchemeType {
    ApiKey,
    Http,
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

/// Builder trait for types that can be converted to OpenAPI Schema
pub trait SchemaBuilder: Sized {
    // This trait is used as a marker for derive macro
    // The actual schema conversion will be implemented separately
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
        match *items {
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
}
