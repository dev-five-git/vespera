//! Metadata collection and storage for routes and schemas

use serde::{Deserialize, Serialize};

/// Route metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteMetadata {
    /// HTTP method
    pub method: String,
    /// Route path
    pub path: String,
    /// Function name
    pub function_name: String,
    /// Module path
    pub module_path: String,
    /// File path
    pub file_path: String,
    /// Function signature (as string for serialization)
    pub signature: String,
    /// Additional error status codes from error_status attribute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_status: Option<Vec<u16>>,
    /// Tags for OpenAPI grouping
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Description for OpenAPI (from route attribute or doc comment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Struct metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructMetadata {
    /// Struct name
    pub name: String,
    /// Struct definition (as string for serialization)
    pub definition: String,
    /// Whether to include in OpenAPI spec (components/schemas)
    /// - true: from #[derive(Schema)] - appears in openapi.json (DEFAULT)
    /// - false: from cross-file lookup - only for schema_type! source, NOT in openapi.json
    #[serde(default = "default_include_in_openapi")]
    pub include_in_openapi: bool,
}

fn default_include_in_openapi() -> bool {
    true
}

impl Default for StructMetadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            definition: String::new(),
            include_in_openapi: true, // Default to true (appears in OpenAPI)
        }
    }
}

impl StructMetadata {
    /// Create a new StructMetadata with include_in_openapi defaulting to true
    pub fn new(name: String, definition: String) -> Self {
        Self {
            name,
            definition,
            include_in_openapi: true,
        }
    }

    /// Create a new StructMetadata for model types (not included in OpenAPI)
    pub fn new_model(name: String, definition: String) -> Self {
        Self {
            name,
            definition,
            include_in_openapi: false,
        }
    }
}

/// Collected metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedMetadata {
    /// Routes
    pub routes: Vec<RouteMetadata>,
    /// Structs
    pub structs: Vec<StructMetadata>,
}

impl CollectedMetadata {
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            structs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_struct_metadata_new() {
        let meta = StructMetadata::new("User".to_string(), "struct User {}".to_string());
        assert_eq!(meta.name, "User");
        assert_eq!(meta.definition, "struct User {}");
        assert!(meta.include_in_openapi); // Should default to true
    }

    #[test]
    fn test_struct_metadata_new_model() {
        let meta = StructMetadata::new_model("Model".to_string(), "struct Model {}".to_string());
        assert_eq!(meta.name, "Model");
        assert_eq!(meta.definition, "struct Model {}");
        assert!(!meta.include_in_openapi); // Should be false for models
    }

    #[test]
    fn test_struct_metadata_default() {
        let meta = StructMetadata::default();
        assert_eq!(meta.name, "");
        assert_eq!(meta.definition, "");
        assert!(meta.include_in_openapi); // Default to true
    }

    #[test]
    fn test_struct_metadata_serde_with_include_in_openapi() {
        let json = r#"{"name":"User","definition":"struct User {}","include_in_openapi":false}"#;
        let meta: StructMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.name, "User");
        assert!(!meta.include_in_openapi);
    }

    #[test]
    fn test_struct_metadata_serde_without_include_in_openapi() {
        // This triggers the default_include_in_openapi() function (lines 45-46)
        let json = r#"{"name":"User","definition":"struct User {}"}"#;
        let meta: StructMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.name, "User");
        assert!(meta.include_in_openapi); // Should default to true via serde default
    }

    #[test]
    fn test_struct_metadata_roundtrip() {
        let original =
            StructMetadata::new("Test".to_string(), "struct Test { x: i32 }".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let restored: StructMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(original.name, restored.name);
        assert_eq!(original.definition, restored.definition);
        assert_eq!(original.include_in_openapi, restored.include_in_openapi);
    }

    #[test]
    fn test_collected_metadata_new() {
        let meta = CollectedMetadata::new();
        assert!(meta.routes.is_empty());
        assert!(meta.structs.is_empty());
    }
}
