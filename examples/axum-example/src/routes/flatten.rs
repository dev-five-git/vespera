//! Example demonstrating #[serde(flatten)] support in Vespera.
//!
//! This module shows how to use serde's flatten attribute to compose
//! schemas using OpenAPI's allOf mechanism.

use serde::{Deserialize, Serialize};
use vespera::{Schema, axum::Json};

/// Common pagination parameters that can be reused across requests
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Pagination {
    /// Page number (1-indexed)
    #[serde(default = "default_page")]
    pub page: i32,
    /// Items per page
    #[serde(default = "default_per_page")]
    pub per_page: i32,
}

fn default_page() -> i32 {
    1
}
fn default_per_page() -> i32 {
    20
}

/// Common metadata for responses
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ResponseMeta {
    /// Total number of items
    pub total: i64,
    /// Whether there are more pages
    pub has_more: bool,
}

/// Request with flattened pagination parameters
///
/// The pagination fields (page, per_page) are merged into this struct's JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct UserListRequest {
    /// Filter users by name (optional)
    pub filter: Option<String>,
    /// Sort order: "asc" or "desc"
    #[serde(default = "default_sort")]
    pub sort: String,
    /// Pagination parameters (flattened into request body)
    #[serde(flatten)]
    pub pagination: Pagination,
}

fn default_sort() -> String {
    "asc".to_string()
}

/// Simple user representation
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct UserItem {
    pub id: i32,
    pub name: String,
    pub email: String,
}

/// Paginated response with flattened metadata
///
/// The response meta fields (total, has_more) are merged into this struct's JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct UserListResponse {
    /// List of users
    pub data: Vec<UserItem>,
    /// Response metadata (flattened into response body)
    #[serde(flatten)]
    pub meta: ResponseMeta,
}

/// List users with pagination (demonstrates flatten for request/response)
///
/// The request accepts flattened pagination parameters (page, per_page)
/// and returns a response with flattened metadata (total, has_more).
#[vespera::route(post, tags = ["flatten"])]
pub async fn list_users(Json(req): Json<UserListRequest>) -> Json<UserListResponse> {
    let users = vec![
        UserItem {
            id: 1,
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
        },
        UserItem {
            id: 2,
            name: "Bob".to_string(),
            email: "bob@example.com".to_string(),
        },
    ];

    Json(UserListResponse {
        data: users,
        meta: ResponseMeta {
            total: 100,
            has_more: req.pagination.page * req.pagination.per_page < 100,
        },
    })
}

/// Request combining multiple flattened structs
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSearchRequest {
    /// Search query string
    pub query: String,
    /// Pagination parameters
    #[serde(flatten)]
    pub pagination: Pagination,
}

/// Response combining multiple flattened structs
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// Search results
    pub results: Vec<String>,
    /// Whether any results were found
    pub found: bool,
    /// Response metadata
    #[serde(flatten)]
    pub meta: ResponseMeta,
}

/// Advanced search endpoint with multiple flatten fields
#[vespera::route(post, path = "/search", tags = ["flatten"])]
pub async fn advanced_search(Json(req): Json<AdvancedSearchRequest>) -> Json<SearchResponse> {
    let results: Vec<String> = vec![
        format!("Result 1 for '{}'", req.query),
        format!("Result 2 for '{}'", req.query),
    ];

    Json(SearchResponse {
        found: !results.is_empty(),
        results,
        meta: ResponseMeta {
            total: 2,
            has_more: false,
        },
    })
}
