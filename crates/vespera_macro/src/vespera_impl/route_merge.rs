use std::collections::HashMap;

use crate::{metadata::CollectedMetadata, route_impl::StoredRouteInfo};

/// Supplement collector's `RouteMetadata` with data from `ROUTE_STORAGE`.
///
/// `#[route]` stores metadata at attribute expansion time.
/// `collector.rs` re-parses the same data from file ASTs.
/// This function merges ROUTE_STORAGE data into collector's output,
/// preferring ROUTE_STORAGE values when they provide richer info.
///
/// Matching is by function name. If multiple routes share a function name,
/// the match is ambiguous and ROUTE_STORAGE data is skipped for safety.
pub(super) fn merge_route_storage_data(
    metadata: &mut CollectedMetadata,
    route_storage: &[StoredRouteInfo],
) {
    if route_storage.is_empty() {
        return;
    }

    // Build `fn_name -> Option<&StoredRouteInfo>` index in a single pass:
    // `Some(_)` when the name is unique, `None` when it is ambiguous
    // (appears more than once).  This turns the previous O(N*M) nested
    // scan into O(N + M).
    let mut stored_index: HashMap<&str, Option<&StoredRouteInfo>> =
        HashMap::with_capacity(route_storage.len());
    for stored in route_storage {
        stored_index
            .entry(stored.fn_name.as_str())
            .and_modify(|slot| *slot = None)
            .or_insert(Some(stored));
    }

    for route in &mut metadata.routes {
        // Skip if no match or ambiguous (multiple routes share fn_name).
        let Some(Some(stored)) = stored_index.get(route.function_name.as_str()) else {
            continue;
        };

        // Supplement with ROUTE_STORAGE data — only override when an
        // explicit value is present.
        if let Some(ref tags) = stored.tags {
            route.tags = Some(tags.clone());
        }
        if let Some(ref desc) = stored.description {
            route.description = Some(desc.clone());
        }
        if let Some(ref status) = stored.error_status {
            route.error_status = Some(status.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::RouteMetadata;

    // ========== Tests for merge_route_storage_data ==========

    #[test]
    fn test_merge_route_storage_empty_storage() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        merge_route_storage_data(&mut metadata, &[]);
        // No changes when storage is empty
        assert!(metadata.routes[0].tags.is_none());
        assert!(metadata.routes[0].description.is_none());
        assert!(metadata.routes[0].error_status.is_none());
    }

    #[test]
    fn test_merge_route_storage_matching_route() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400, 404]),
            tags: Some(vec!["users".to_string()]),
            description: Some("List all users".to_string()),
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        assert_eq!(metadata.routes[0].tags, Some(vec!["users".to_string()]));
        assert_eq!(
            metadata.routes[0].description,
            Some("List all users".to_string())
        );
        assert_eq!(metadata.routes[0].error_status, Some(vec![400, 404]));
    }

    #[test]
    fn test_merge_route_storage_no_match() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "create_user".to_string(),
            method: Some("post".to_string()),
            custom_path: None,
            error_status: Some(vec![400]),
            tags: Some(vec!["users".to_string()]),
            description: None,
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        // No match — fields unchanged
        assert!(metadata.routes[0].tags.is_none());
        assert!(metadata.routes[0].error_status.is_none());
    }

    #[test]
    fn test_merge_route_storage_ambiguous_skipped() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "handler".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: None,
            tags: None,
            description: None,
        });

        // Two StoredRouteInfo with same fn_name — ambiguous
        let storage = vec![
            StoredRouteInfo {
                fn_name: "handler".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                tags: Some(vec!["file-a".to_string()]),
                description: None,
                fn_item_str: String::new(),
                file_path: None,
            },
            StoredRouteInfo {
                fn_name: "handler".to_string(),
                method: Some("post".to_string()),
                custom_path: None,
                error_status: None,
                tags: Some(vec!["file-b".to_string()]),
                description: None,
                fn_item_str: String::new(),
                file_path: None,
            },
        ];

        merge_route_storage_data(&mut metadata, &storage);
        // Ambiguous match — no merge
        assert!(metadata.routes[0].tags.is_none());
    }

    #[test]
    fn test_merge_route_storage_preserves_existing() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: Some(vec![500]),
            tags: Some(vec!["existing-tag".to_string()]),
            description: Some("Existing description".to_string()),
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400, 404]),
            tags: Some(vec!["new-tag".to_string()]),
            description: Some("New description".to_string()),
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        // ROUTE_STORAGE values override when they have explicit values
        assert_eq!(metadata.routes[0].tags, Some(vec!["new-tag".to_string()]));
        assert_eq!(
            metadata.routes[0].description,
            Some("New description".to_string())
        );
        assert_eq!(metadata.routes[0].error_status, Some(vec![400, 404]));
    }

    #[test]
    fn test_merge_route_storage_partial_fields() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "get_users".to_string(),
            module_path: "routes".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: None,
            tags: Some(vec!["from-collector".to_string()]),
            description: Some("From doc comment".to_string()),
        });

        // StoredRouteInfo with only error_status (tags/description are None)
        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400]),
            tags: None,
            description: None,
            fn_item_str: String::new(),
            file_path: None,
        }];

        merge_route_storage_data(&mut metadata, &storage);
        // Only error_status should be set; tags and description preserved from collector
        assert_eq!(
            metadata.routes[0].tags,
            Some(vec!["from-collector".to_string()])
        );
        assert_eq!(
            metadata.routes[0].description,
            Some("From doc comment".to_string())
        );
        assert_eq!(metadata.routes[0].error_status, Some(vec![400]));
    }
}
