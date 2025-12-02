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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("GET", HttpMethod::Get)]
    #[case("get", HttpMethod::Get)]
    #[case("Get", HttpMethod::Get)]
    #[case("POST", HttpMethod::Post)]
    #[case("post", HttpMethod::Post)]
    #[case("Post", HttpMethod::Post)]
    #[case("PUT", HttpMethod::Put)]
    #[case("put", HttpMethod::Put)]
    #[case("Put", HttpMethod::Put)]
    #[case("PATCH", HttpMethod::Patch)]
    #[case("patch", HttpMethod::Patch)]
    #[case("Patch", HttpMethod::Patch)]
    #[case("DELETE", HttpMethod::Delete)]
    #[case("delete", HttpMethod::Delete)]
    #[case("Delete", HttpMethod::Delete)]
    #[case("HEAD", HttpMethod::Head)]
    #[case("head", HttpMethod::Head)]
    #[case("Head", HttpMethod::Head)]
    #[case("OPTIONS", HttpMethod::Options)]
    #[case("options", HttpMethod::Options)]
    #[case("Options", HttpMethod::Options)]
    #[case("TRACE", HttpMethod::Trace)]
    #[case("trace", HttpMethod::Trace)]
    #[case("Trace", HttpMethod::Trace)]
    fn test_http_method_from_str(#[case] input: &str, #[case] expected: HttpMethod) {
        let result = HttpMethod::from(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_http_method_from_invalid_str() {
        // Invalid method should default to Get
        let result = HttpMethod::from("INVALID");
        assert_eq!(result, HttpMethod::Get);
    }

    #[test]
    fn test_http_method_serialization() {
        // Test serde serialization (should be UPPERCASE)
        let method = HttpMethod::Get;
        let serialized = serde_json::to_string(&method).unwrap();
        assert_eq!(serialized, "\"GET\"");

        let method = HttpMethod::Post;
        let serialized = serde_json::to_string(&method).unwrap();
        assert_eq!(serialized, "\"POST\"");

        let method = HttpMethod::Delete;
        let serialized = serde_json::to_string(&method).unwrap();
        assert_eq!(serialized, "\"DELETE\"");
    }

    #[test]
    fn test_http_method_deserialization() {
        // Test serde deserialization
        let method: HttpMethod = serde_json::from_str("\"GET\"").unwrap();
        assert_eq!(method, HttpMethod::Get);

        let method: HttpMethod = serde_json::from_str("\"POST\"").unwrap();
        assert_eq!(method, HttpMethod::Post);

        let method: HttpMethod = serde_json::from_str("\"DELETE\"").unwrap();
        assert_eq!(method, HttpMethod::Delete);
    }

    #[test]
    fn test_path_item_set_operation() {
        let mut path_item = PathItem {
            get: None,
            post: None,
            put: None,
            patch: None,
            delete: None,
            head: None,
            options: None,
            trace: None,
            parameters: None,
            summary: None,
            description: None,
        };

        let operation = Operation {
            operation_id: Some("test_operation".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
        };

        // Test setting GET operation
        path_item.set_operation(HttpMethod::Get, operation.clone());
        assert!(path_item.get.is_some());
        assert_eq!(
            path_item.get.as_ref().unwrap().operation_id,
            Some("test_operation".to_string())
        );

        // Test setting POST operation
        let mut operation_post = operation.clone();
        operation_post.operation_id = Some("post_operation".to_string());
        path_item.set_operation(HttpMethod::Post, operation_post);
        assert!(path_item.post.is_some());
        assert_eq!(
            path_item.post.as_ref().unwrap().operation_id,
            Some("post_operation".to_string())
        );

        // Test setting PUT operation
        let mut operation_put = operation.clone();
        operation_put.operation_id = Some("put_operation".to_string());
        path_item.set_operation(HttpMethod::Put, operation_put);
        assert!(path_item.put.is_some());

        // Test setting PATCH operation
        let mut operation_patch = operation.clone();
        operation_patch.operation_id = Some("patch_operation".to_string());
        path_item.set_operation(HttpMethod::Patch, operation_patch);
        assert!(path_item.patch.is_some());

        // Test setting DELETE operation
        let mut operation_delete = operation.clone();
        operation_delete.operation_id = Some("delete_operation".to_string());
        path_item.set_operation(HttpMethod::Delete, operation_delete);
        assert!(path_item.delete.is_some());

        // Test setting HEAD operation
        let mut operation_head = operation.clone();
        operation_head.operation_id = Some("head_operation".to_string());
        path_item.set_operation(HttpMethod::Head, operation_head);
        assert!(path_item.head.is_some());

        // Test setting OPTIONS operation
        let mut operation_options = operation.clone();
        operation_options.operation_id = Some("options_operation".to_string());
        path_item.set_operation(HttpMethod::Options, operation_options);
        assert!(path_item.options.is_some());

        // Test setting TRACE operation
        let mut operation_trace = operation;
        operation_trace.operation_id = Some("trace_operation".to_string());
        path_item.set_operation(HttpMethod::Trace, operation_trace);
        assert!(path_item.trace.is_some());
    }

    #[test]
    fn test_path_item_get_operation() {
        let mut path_item = PathItem {
            get: None,
            post: None,
            put: None,
            patch: None,
            delete: None,
            head: None,
            options: None,
            trace: None,
            parameters: None,
            summary: None,
            description: None,
        };

        let operation = Operation {
            operation_id: Some("test_operation".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
        };

        // Initially, all operations should be None
        assert!(path_item.get_operation(&HttpMethod::Get).is_none());
        assert!(path_item.get_operation(&HttpMethod::Post).is_none());

        // Set GET operation
        path_item.set_operation(HttpMethod::Get, operation.clone());
        let retrieved = path_item.get_operation(&HttpMethod::Get);
        assert!(retrieved.is_some());
        assert_eq!(
            retrieved.unwrap().operation_id,
            Some("test_operation".to_string())
        );

        // Set POST operation
        let mut operation_post = operation.clone();
        operation_post.operation_id = Some("post_operation".to_string());
        path_item.set_operation(HttpMethod::Post, operation_post);
        let retrieved = path_item.get_operation(&HttpMethod::Post);
        assert!(retrieved.is_some());
        assert_eq!(
            retrieved.unwrap().operation_id,
            Some("post_operation".to_string())
        );

        // Test all methods
        path_item.set_operation(HttpMethod::Put, operation.clone());
        assert!(path_item.get_operation(&HttpMethod::Put).is_some());

        path_item.set_operation(HttpMethod::Patch, operation.clone());
        assert!(path_item.get_operation(&HttpMethod::Patch).is_some());

        path_item.set_operation(HttpMethod::Delete, operation.clone());
        assert!(path_item.get_operation(&HttpMethod::Delete).is_some());

        path_item.set_operation(HttpMethod::Head, operation.clone());
        assert!(path_item.get_operation(&HttpMethod::Head).is_some());

        path_item.set_operation(HttpMethod::Options, operation.clone());
        assert!(path_item.get_operation(&HttpMethod::Options).is_some());

        path_item.set_operation(HttpMethod::Trace, operation);
        assert!(path_item.get_operation(&HttpMethod::Trace).is_some());
    }

    #[test]
    fn test_path_item_set_operation_overwrites() {
        let mut path_item = PathItem {
            get: None,
            post: None,
            put: None,
            patch: None,
            delete: None,
            head: None,
            options: None,
            trace: None,
            parameters: None,
            summary: None,
            description: None,
        };

        let operation1 = Operation {
            operation_id: Some("first".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
        };

        let operation2 = Operation {
            operation_id: Some("second".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
        };

        // Set first operation
        path_item.set_operation(HttpMethod::Get, operation1);
        assert_eq!(
            path_item.get.as_ref().unwrap().operation_id,
            Some("first".to_string())
        );

        // Overwrite with second operation
        path_item.set_operation(HttpMethod::Get, operation2);
        assert_eq!(
            path_item.get.as_ref().unwrap().operation_id,
            Some("second".to_string())
        );
    }

    #[test]
    fn test_http_method_equality() {
        let method1 = HttpMethod::Get;
        let method2 = HttpMethod::Get;
        let method3 = HttpMethod::Post;

        assert_eq!(method1, method2);
        assert_ne!(method1, method3);
    }

    #[test]
    fn test_http_method_clone() {
        let method = HttpMethod::Get;
        let cloned = method.clone();
        assert_eq!(method, cloned);
    }

    #[test]
    fn test_http_method_hash() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        map.insert(HttpMethod::Get, "GET method");
        map.insert(HttpMethod::Post, "POST method");

        assert_eq!(map.get(&HttpMethod::Get), Some(&"GET method"));
        assert_eq!(map.get(&HttpMethod::Post), Some(&"POST method"));
        assert_eq!(map.get(&HttpMethod::Put), None);
    }
}
