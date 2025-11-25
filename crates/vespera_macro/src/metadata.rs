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
}

/// Struct metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructMetadata {
    /// Struct name
    pub name: String,
    /// Module path
    pub module_path: String,
    /// File path
    pub file_path: String,
    /// Struct definition (as string for serialization)
    pub definition: String,
}

/// Collected metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedMetadata {
    /// Folder name used for collection
    pub folder_name: String,
    /// Routes
    pub routes: Vec<RouteMetadata>,
    /// Structs
    pub structs: Vec<StructMetadata>,
}

impl CollectedMetadata {
    pub fn new(folder_name: String) -> Self {
        Self {
            folder_name,
            routes: Vec::new(),
            structs: Vec::new(),
        }
    }
}

impl Default for CollectedMetadata {
    fn default() -> Self {
        Self::new("routes".to_string())
    }
}

