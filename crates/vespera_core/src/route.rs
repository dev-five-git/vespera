//! Route-related structure definitions

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::SchemaRef;

/// HTTP method
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Trace,
}

impl From<&str> for HttpMethod {
    fn from(value: &str) -> Self {
        match value.to_uppercase().as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            "HEAD" => HttpMethod::Head,
            "OPTIONS" => HttpMethod::Options,
            "TRACE" => HttpMethod::Trace,
            _ => HttpMethod::Get, // default value
        }
    }
}

/// Parameter location in the request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterLocation {
    Query,
    Header,
    Path,
    Cookie,
}

/// Parameter definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    /// Parameter name
    pub name: String,
    /// Parameter location
    pub r#in: ParameterLocation,
    /// Parameter description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the parameter is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Schema reference or inline schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
    /// Example value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,
}

/// Request body definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestBody {
    /// Request body description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the request body is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Schema per Content-Type
    pub content: BTreeMap<String, MediaType>,
}

/// Media type definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaType {
    /// Schema reference or inline schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
    /// Example
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,
    /// Examples
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<HashMap<String, Example>>,
}

/// Example definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Example {
    /// Example summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Example description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Example value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// Response definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// Response description
    pub description: String,
    /// Header definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, Header>>,
    /// Schema per Content-Type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, MediaType>>,
}

/// Header definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    /// Header description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Schema reference or inline schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
}

/// OpenAPI Operation definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    /// Operation ID (unique identifier)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// List of tags
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// List of parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    /// Request body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    /// Response definitions (status code -> Response)
    pub responses: BTreeMap<String, Response>,
    /// Security requirements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<HashMap<String, Vec<String>>>>,
}

/// Path Item definition (all HTTP methods for a specific path)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathItem {
    /// GET method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    /// POST method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
    /// PUT method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<Operation>,
    /// PATCH method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Operation>,
    /// DELETE method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<Operation>,
    /// HEAD method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<Operation>,
    /// OPTIONS method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Operation>,
    /// TRACE method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Operation>,
    /// Path parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    /// Summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PathItem {
    /// Set an operation for a specific HTTP method
    pub fn set_operation(&mut self, method: HttpMethod, operation: Operation) {
        match method {
            HttpMethod::Get => self.get = Some(operation),
            HttpMethod::Post => self.post = Some(operation),
            HttpMethod::Put => self.put = Some(operation),
            HttpMethod::Patch => self.patch = Some(operation),
            HttpMethod::Delete => self.delete = Some(operation),
            HttpMethod::Head => self.head = Some(operation),
            HttpMethod::Options => self.options = Some(operation),
            HttpMethod::Trace => self.trace = Some(operation),
        }
    }

    /// Get an operation for a specific HTTP method
    pub fn get_operation(&self, method: &HttpMethod) -> Option<&Operation> {
        match method {
            HttpMethod::Get => self.get.as_ref(),
            HttpMethod::Post => self.post.as_ref(),
            HttpMethod::Put => self.put.as_ref(),
            HttpMethod::Patch => self.patch.as_ref(),
            HttpMethod::Delete => self.delete.as_ref(),
            HttpMethod::Head => self.head.as_ref(),
            HttpMethod::Options => self.options.as_ref(),
            HttpMethod::Trace => self.trace.as_ref(),
        }
    }
}

/// Route information (for internal use)
#[derive(Debug, Clone)]
pub struct RouteInfo {
    /// HTTP method
    pub method: HttpMethod,
    /// Path
    pub path: String,
    /// Operation information
    pub operation: Operation,
}
