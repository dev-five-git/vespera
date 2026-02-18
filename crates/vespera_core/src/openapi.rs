//! `OpenAPI` document structure definitions

use crate::route::PathItem;
use crate::schema::{Components, ExternalDocumentation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// `OpenAPI` document version
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OpenApiVersion {
    #[serde(rename = "3.0.0")]
    V3_0_0,
    #[serde(rename = "3.0.1")]
    V3_0_1,
    #[serde(rename = "3.0.2")]
    V3_0_2,
    #[serde(rename = "3.0.3")]
    V3_0_3,
    #[serde(rename = "3.1.0")]
    #[default]
    V3_1_0,
}

/// Contact information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// Contact name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Contact URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Contact email
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// License information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    /// License name
    pub name: String,
    /// License URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// API information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// API title
    pub title: String,
    /// API version
    pub version: String,
    /// API description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Terms of service URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terms_of_service: Option<String>,
    /// Contact information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<Contact>,
    /// License information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
    /// Summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Server variable
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVariable {
    /// Default value
    pub default: String,
    /// Enum values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<String>>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Server information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    /// Server URL
    pub url: String,
    /// Server description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Server variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<HashMap<String, ServerVariable>>,
}

/// Tag definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    /// Tag name
    pub name: String,
    /// Tag description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// External documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,
}

/// `OpenAPI` document (root structure)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApi {
    /// `OpenAPI` version
    pub openapi: OpenApiVersion,
    /// API information
    pub info: Info,
    /// Server list
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<Server>>,
    /// Path definitions
    pub paths: BTreeMap<String, PathItem>,
    /// Components (reusable components)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Components>,
    /// Security requirements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<HashMap<String, Vec<String>>>>,
    /// Tag definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<Tag>>,
    /// External documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,
}

impl OpenApi {
    /// Merge another `OpenAPI` document into this one.
    /// Paths, schemas, and tags from `other` are added to `self`.
    /// If there are conflicts, `self` takes precedence.
    pub fn merge(&mut self, other: Self) {
        // Merge paths (self takes precedence on conflict)
        for (path, item) in other.paths {
            self.paths.entry(path).or_insert(item);
        }

        // Merge components
        if let Some(other_components) = other.components {
            let self_components = self.components.get_or_insert(Components {
                schemas: None,
                responses: None,
                parameters: None,
                examples: None,
                request_bodies: None,
                headers: None,
                security_schemes: None,
            });

            // Merge schemas
            if let Some(other_schemas) = other_components.schemas {
                let self_schemas = self_components.schemas.get_or_insert_with(BTreeMap::new);
                for (name, schema) in other_schemas {
                    self_schemas.entry(name).or_insert(schema);
                }
            }

            // Merge security schemes
            if let Some(other_security_schemes) = other_components.security_schemes {
                let self_security_schemes = self_components
                    .security_schemes
                    .get_or_insert_with(HashMap::new);
                for (name, scheme) in other_security_schemes {
                    self_security_schemes.entry(name).or_insert(scheme);
                }
            }
        }

        // Merge tags (deduplicate by name)
        if let Some(other_tags) = other.tags {
            let self_tags = self.tags.get_or_insert_with(Vec::new);
            for tag in other_tags {
                if !self_tags.iter().any(|t| t.name == tag.name) {
                    self_tags.push(tag);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::{Operation, PathItem};
    use crate::schema::{Components, Schema, SchemaType, SecurityScheme, SecuritySchemeType};

    fn create_base_openapi() -> OpenApi {
        OpenApi {
            openapi: OpenApiVersion::V3_1_0,
            info: Info {
                title: "Base API".to_string(),
                version: "1.0.0".to_string(),
                description: None,
                terms_of_service: None,
                contact: None,
                license: None,
                summary: None,
            },
            servers: None,
            paths: BTreeMap::new(),
            components: None,
            security: None,
            tags: None,
            external_docs: None,
        }
    }

    fn create_path_item(summary: &str) -> PathItem {
        PathItem {
            get: Some(Operation {
                summary: Some(summary.to_string()),
                description: None,
                operation_id: None,
                tags: None,
                parameters: None,
                request_body: None,
                responses: BTreeMap::new(),
                security: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_merge_paths() {
        let mut base = create_base_openapi();
        base.paths
            .insert("/users".to_string(), create_path_item("Get users"));

        let mut other = create_base_openapi();
        other
            .paths
            .insert("/posts".to_string(), create_path_item("Get posts"));
        other
            .paths
            .insert("/users".to_string(), create_path_item("Other users")); // Conflict

        base.merge(other);

        // Both paths should exist
        assert!(base.paths.contains_key("/users"));
        assert!(base.paths.contains_key("/posts"));
        // Self takes precedence on conflict
        assert_eq!(
            base.paths
                .get("/users")
                .unwrap()
                .get
                .as_ref()
                .unwrap()
                .summary,
            Some("Get users".to_string())
        );
    }

    #[test]
    fn test_merge_schemas() {
        let mut base = create_base_openapi();
        let mut base_schemas = BTreeMap::new();
        base_schemas.insert("User".to_string(), Schema::object());
        base.components = Some(Components {
            schemas: Some(base_schemas),
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        let mut other = create_base_openapi();
        let mut other_schemas = BTreeMap::new();
        other_schemas.insert("Post".to_string(), Schema::object());
        other_schemas.insert("User".to_string(), Schema::string()); // Conflict
        other.components = Some(Components {
            schemas: Some(other_schemas),
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        base.merge(other);

        let schemas = base.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("User"));
        assert!(schemas.contains_key("Post"));
        // Self takes precedence on conflict
        assert_eq!(
            schemas.get("User").unwrap().schema_type,
            Some(SchemaType::Object)
        );
    }

    #[test]
    fn test_merge_schemas_when_self_has_no_components() {
        let mut base = create_base_openapi();
        assert!(base.components.is_none());

        let mut other = create_base_openapi();
        let mut other_schemas = BTreeMap::new();
        other_schemas.insert("Post".to_string(), Schema::object());
        other.components = Some(Components {
            schemas: Some(other_schemas),
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: None,
        });

        base.merge(other);

        assert!(base.components.is_some());
        let schemas = base.components.as_ref().unwrap().schemas.as_ref().unwrap();
        assert!(schemas.contains_key("Post"));
    }

    #[test]
    fn test_merge_security_schemes() {
        let mut base = create_base_openapi();
        let mut base_security_schemes = HashMap::new();
        base_security_schemes.insert(
            "bearerAuth".to_string(),
            SecurityScheme {
                r#type: SecuritySchemeType::Http,
                description: None,
                name: None,
                r#in: None,
                scheme: Some("bearer".to_string()),
                bearer_format: Some("JWT".to_string()),
            },
        );
        base.components = Some(Components {
            schemas: None,
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: Some(base_security_schemes),
        });

        let mut other = create_base_openapi();
        let mut other_security_schemes = HashMap::new();
        other_security_schemes.insert(
            "apiKey".to_string(),
            SecurityScheme {
                r#type: SecuritySchemeType::ApiKey,
                description: None,
                name: Some("X-API-Key".to_string()),
                r#in: Some("header".to_string()),
                scheme: None,
                bearer_format: None,
            },
        );
        other.components = Some(Components {
            schemas: None,
            responses: None,
            parameters: None,
            examples: None,
            request_bodies: None,
            headers: None,
            security_schemes: Some(other_security_schemes),
        });

        base.merge(other);

        let security_schemes = base
            .components
            .as_ref()
            .unwrap()
            .security_schemes
            .as_ref()
            .unwrap();
        assert!(security_schemes.contains_key("bearerAuth"));
        assert!(security_schemes.contains_key("apiKey"));
    }

    #[test]
    fn test_merge_tags() {
        let mut base = create_base_openapi();
        base.tags = Some(vec![Tag {
            name: "users".to_string(),
            description: Some("User operations".to_string()),
            external_docs: None,
        }]);

        let mut other = create_base_openapi();
        other.tags = Some(vec![
            Tag {
                name: "posts".to_string(),
                description: Some("Post operations".to_string()),
                external_docs: None,
            },
            Tag {
                name: "users".to_string(),
                description: Some("Duplicate users tag".to_string()),
                external_docs: None,
            }, // Duplicate
        ]);

        base.merge(other);

        let tags = base.tags.as_ref().unwrap();
        assert_eq!(tags.len(), 2); // No duplicates
        assert!(tags.iter().any(|t| t.name == "users"));
        assert!(tags.iter().any(|t| t.name == "posts"));
        // Self's description takes precedence
        let users_tag = tags.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(users_tag.description, Some("User operations".to_string()));
    }

    #[test]
    fn test_merge_tags_when_self_has_none() {
        let mut base = create_base_openapi();
        assert!(base.tags.is_none());

        let mut other = create_base_openapi();
        other.tags = Some(vec![Tag {
            name: "posts".to_string(),
            description: None,
            external_docs: None,
        }]);

        base.merge(other);

        assert!(base.tags.is_some());
        assert_eq!(base.tags.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_merge_empty_other() {
        let mut base = create_base_openapi();
        base.paths
            .insert("/users".to_string(), create_path_item("Get users"));
        base.tags = Some(vec![Tag {
            name: "users".to_string(),
            description: None,
            external_docs: None,
        }]);

        let other = create_base_openapi(); // Empty paths, no components, no tags

        base.merge(other);

        // Base should remain unchanged
        assert_eq!(base.paths.len(), 1);
        assert!(base.paths.contains_key("/users"));
        assert_eq!(base.tags.as_ref().unwrap().len(), 1);
    }
}
