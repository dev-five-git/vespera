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
            has_more: has_more(req.pagination.page, req.pagination.per_page),
        },
    })
}

/// Whether the page starting at `page` of `per_page` items leaves users out
/// of the 100 (`total`). The document puts no minimum on either value, so a
/// page or size below 1 is read as 1 (`page` is 1-indexed) rather than
/// reporting more results for a page that does not exist. Widened: two
/// client-supplied `i32`s may overflow their product.
fn has_more(page: i32, per_page: i32) -> bool {
    i64::from(page.max(1)) * i64::from(per_page.max(1)) < 100
}

#[cfg(test)]
mod tests {
    use super::has_more;

    /// Values below 1 are read as 1: page 0 is the first page, and a size of
    /// 0 is a size of 1 (`has_more(0, 200)` is the first page of 200 items,
    /// which leaves users out, not "page 0" of an unbounded list).
    #[test]
    fn values_below_one_are_read_as_one() {
        assert_eq!(has_more(0, 20), has_more(1, 20));
        assert_eq!(has_more(-3, 20), has_more(1, 20));
        assert_eq!(has_more(0, 200), has_more(1, 200));
        assert!(!has_more(0, 200));
        assert_eq!(has_more(1, 0), has_more(1, 1));
        assert!(has_more(1, -7));
    }

    #[test]
    fn has_more_compares_the_product_with_the_total() {
        assert!(has_more(4, 24));
        assert!(!has_more(5, 25));
        // Two large values overflow an `i32` product; the widened one stays
        // exact.
        assert!(!has_more(i32::MAX, i32::MAX));
    }
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
