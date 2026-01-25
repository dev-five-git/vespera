//! OpenAPI document structure definitions

use crate::route::PathItem;
use crate::schema::{Components, ExternalDocumentation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// OpenAPI document version
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// OpenAPI document (root structure)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApi {
    /// OpenAPI version
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
    /// Merge another OpenAPI document into this one.
    /// Paths, schemas, and tags from `other` are added to `self`.
    /// If there are conflicts, `self` takes precedence.
    pub fn merge(&mut self, other: OpenApi) {
        // Merge paths (self takes precedence on conflict)
        for (path, item) in other.paths {
            self.paths.entry(path).or_insert(item);
        }

        // Merge components
        if let Some(other_components) = other.components {
            let self_components = self.components.get_or_insert_with(|| Components {
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

    /// Merge from a JSON string. Returns error if parsing fails.
    pub fn merge_from_str(&mut self, json_str: &str) -> Result<(), serde_json::Error> {
        let other: OpenApi = serde_json::from_str(json_str)?;
        self.merge(other);
        Ok(())
    }
}
