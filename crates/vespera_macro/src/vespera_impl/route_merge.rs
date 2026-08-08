use std::collections::HashMap;

use crate::{
    file_utils::normalize_path_key, metadata::CollectedMetadata, route_impl::StoredRouteInfo,
};

/// Supplement collector's `RouteMetadata` with data from `ROUTE_STORAGE`.
///
/// `#[route]` stores metadata at attribute expansion time.
/// `collector.rs` re-parses the same data from file ASTs.
/// This function merges ROUTE_STORAGE data into collector's output,
/// preferring ROUTE_STORAGE values when they provide richer info.
///
/// Matching is by normalized `(file_path, function_name)`. Legacy storage entries
/// without a file path only match when their function name is unambiguous.
pub(super) fn merge_route_storage_data(
    metadata: &mut CollectedMetadata,
    route_storage: &[StoredRouteInfo],
) {
    if route_storage.is_empty() {
        return;
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let mut stored_by_path: HashMap<(String, &str), &StoredRouteInfo> =
        HashMap::with_capacity(route_storage.len());
    let mut fallback_by_name: HashMap<&str, Option<&StoredRouteInfo>> =
        HashMap::with_capacity(route_storage.len());
    for stored in route_storage {
        if let Some(file_path) = &stored.file_path {
            stored_by_path.insert(
                (normalize_path_key(file_path, &cwd), stored.fn_name.as_str()),
                stored,
            );
        }
        fallback_by_name
            .entry(stored.fn_name.as_str())
            .and_modify(|slot| *slot = None)
            .or_insert(Some(stored));
    }

    for route in &mut metadata.routes {
        let route_key = (
            normalize_path_key(&route.file_path, &cwd),
            route.function_name.as_str(),
        );
        let stored = stored_by_path.get(&route_key).copied().or_else(|| {
            fallback_by_name
                .get(route.function_name.as_str())
                .copied()
                .flatten()
        });

        let Some(stored) = stored else {
            continue;
        };

        apply_stored_route(route, stored);
    }
}

/// Copy each listed `Option` field from `$stored` onto `$route`, but only when the
/// stored side actually carries a value.
///
/// Every field named here is an `Option<T>` with the *same* `T` on both
/// [`StoredRouteInfo`] and [`crate::metadata::RouteMetadata`], and each one used to
/// be its own copy-pasted `if let Some(..) = stored.x { route.x = Some(x.clone()) }`
/// block. Keeping the set declarative means a new `#[route]` attribute is one
/// identifier in the invocation below instead of another block that is silently
/// easy to forget (which would drop the attribute from the generated OpenAPI with
/// no compile error).
///
/// The `is_some` guard is load-bearing: an unconditional assignment would clobber
/// collector-derived values (doc comments, inferred tags) with `None`.
macro_rules! copy_if_some {
    ($route:ident, $stored:ident, [$($field:ident),+ $(,)?]) => {
        $(
            if $stored.$field.is_some() {
                $route.$field.clone_from(&$stored.$field);
            }
        )+
    };
}

fn apply_stored_route(route: &mut crate::metadata::RouteMetadata, stored: &StoredRouteInfo) {
    // Supplement with ROUTE_STORAGE data — only override when an explicit value is present.
    copy_if_some!(
        route,
        stored,
        [
            tags,
            security,
            operation_id,
            summary,
            description,
            success_status,
            error_status,
            typed_responses,
            request_example,
            response_example,
        ]
    );

    // Structurally different: a bare `bool` flag and a `Vec` with no `Option` wrapper.
    if stored.deprecated {
        route.deprecated = true;
    }
    if !stored.headers.is_empty() {
        route.headers.clone_from(&stored.headers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::RouteMetadata;

    fn stored_route(fn_name: &str, file_path: Option<&str>, tags: &[&str]) -> StoredRouteInfo {
        StoredRouteInfo {
            fn_name: fn_name.to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: None,
            typed_responses: None,
            tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: String::new(),
            file_path: file_path.map(str::to_string),
        }
    }

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
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
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
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400, 404]),
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: Some("List all users".to_string()),
            fn_sig_str: String::new(),
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
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "create_user".to_string(),
            method: Some("post".to_string()),
            custom_path: None,
            error_status: Some(vec![400]),
            typed_responses: None,
            tags: Some(vec!["users".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: String::new(),
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
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        // Two StoredRouteInfo with same fn_name — ambiguous
        let storage = vec![
            StoredRouteInfo {
                fn_name: "handler".to_string(),
                method: Some("get".to_string()),
                custom_path: None,
                error_status: None,
                typed_responses: None,
                tags: Some(vec!["file-a".to_string()]),
                security: None,
                headers: Vec::new(),
                success_status: None,
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
                fn_sig_str: String::new(),
                file_path: None,
            },
            StoredRouteInfo {
                fn_name: "handler".to_string(),
                method: Some("post".to_string()),
                custom_path: None,
                error_status: None,
                typed_responses: None,
                tags: Some(vec!["file-b".to_string()]),
                security: None,
                headers: Vec::new(),
                success_status: None,
                operation_id: None,
                summary: None,
                request_example: None,
                response_example: None,
                deprecated: false,
                description: None,
                fn_sig_str: String::new(),
                file_path: None,
            },
        ];

        merge_route_storage_data(&mut metadata, &storage);
        // Ambiguous match — no merge
        assert!(metadata.routes[0].tags.is_none());
    }

    #[test]
    fn test_merge_route_storage_disambiguates_same_fn_name_by_file_path() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/users".to_string(),
            function_name: "handler".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "routes/users.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });
        metadata.routes.push(RouteMetadata {
            method: "get".to_string(),
            path: "/posts".to_string(),
            function_name: "handler".to_string(),
            module_path: "routes::posts".to_string(),
            file_path: "routes/posts.rs".to_string(),
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        });

        let storage = vec![
            stored_route("handler", Some("routes/users.rs"), &["users-file"]),
            stored_route("handler", Some("routes/posts.rs"), &["posts-file"]),
        ];

        merge_route_storage_data(&mut metadata, &storage);

        assert_eq!(
            metadata.routes[0].tags,
            Some(vec!["users-file".to_string()])
        );
        assert_eq!(
            metadata.routes[1].tags,
            Some(vec!["posts-file".to_string()])
        );
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
            typed_responses: None,
            tags: Some(vec!["existing-tag".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: Some("Existing description".to_string()),
        });

        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400, 404]),
            typed_responses: None,
            tags: Some(vec!["new-tag".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: Some("New description".to_string()),
            fn_sig_str: String::new(),
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
            typed_responses: None,
            tags: Some(vec!["from-collector".to_string()]),
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: Some("From doc comment".to_string()),
        });

        // StoredRouteInfo with only error_status (tags/description are None)
        let storage = vec![StoredRouteInfo {
            fn_name: "get_users".to_string(),
            method: Some("get".to_string()),
            custom_path: None,
            error_status: Some(vec![400]),
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            success_status: None,
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            fn_sig_str: String::new(),
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
