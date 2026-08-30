use std::collections::{BTreeMap, HashSet};

use syn::{
    LitStr,
    parse::{Parse, ParseStream},
};

use crate::metadata::CollectedMetadata;

/// Input for `export_app`! macro
pub struct ExportAppInput {
    /// App name (struct name to generate)
    pub name: syn::Ident,
    /// Route directory
    pub dir: Option<LitStr>,
    /// Explicit public base path for every exported route
    pub prefix: Option<LitStr>,
}

impl Parse for ExportAppInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;

        let mut dir = None;
        let mut prefix = None;

        // Parse optional comma and arguments
        while input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: syn::Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "dir" => {
                    // Reject a repeated `dir` with a spanned error instead of
                    // silently letting the later value overwrite the earlier
                    // one — matches the `vespera!` arg parser's duplicate guard.
                    if dir.is_some() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "duplicate field `dir` in export_app! macro",
                        ));
                    }
                    input.parse::<syn::Token![=]>()?;
                    dir = Some(input.parse()?);
                }
                "prefix" => {
                    if prefix.is_some() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "duplicate field `prefix` in export_app! macro",
                        ));
                    }
                    input.parse::<syn::Token![=]>()?;
                    let literal: LitStr = input.parse()?;
                    prefix = Some(normalize_prefix(&literal)?);
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown field: `{ident_str}`. Expected `dir` or `prefix`"),
                    ));
                }
            }
        }

        Ok(Self { name, dir, prefix })
    }
}

/// Normalize an explicit export prefix while retaining the literal's span for
/// compile-time diagnostics. Empty and root-only prefixes mean "no prefix".
fn normalize_prefix(prefix: &LitStr) -> syn::Result<LitStr> {
    let raw = prefix.value();
    if raw.chars().any(char::is_whitespace) || raw.contains(['?', '#']) {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must be a URL path without whitespace, a query, or a fragment",
        ));
    }

    let with_leading_slash = if raw.is_empty() || raw.starts_with('/') {
        raw
    } else {
        format!("/{raw}")
    };
    let normalized = with_leading_slash.trim_end_matches('/');
    if normalized.contains("//") {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must not contain empty path segments (`//`)",
        ));
    }
    if !normalized.is_empty() && schema_namespace_from_prefix(normalized).is_empty() {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must contain at least one alphanumeric character",
        ));
    }

    Ok(LitStr::new(normalized, prefix.span()))
}

/// Derive a deterministic PascalCase component namespace from a normalized
/// route prefix. A conventional leading `/api` segment is omitted when more
/// specific segments follow (`/api/media-library` -> `MediaLibrary`).
pub fn schema_namespace_from_prefix(prefix: &str) -> String {
    let segments: Vec<&str> = prefix
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let start = usize::from(segments.len() > 1 && segments[0].eq_ignore_ascii_case("api"));
    let mut namespace = String::new();
    for word in segments[start..]
        .iter()
        .flat_map(|segment| segment.split(|ch: char| !ch.is_alphanumeric()))
    {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            namespace.extend(first.to_uppercase());
            namespace.extend(chars);
        }
    }
    namespace
}

// Apply the normalized prefix to collected route metadata exactly once.
// Both router generation and OpenAPI generation consume this same metadata.
pub fn apply_export_prefix(metadata: &mut CollectedMetadata, prefix: &str) {
    if prefix.is_empty() {
        return;
    }

    for route in &mut metadata.routes {
        route.path = if route.path == "/" {
            prefix.to_owned()
        } else {
            format!("{prefix}{}", route.path)
        };
    }
}

/// Namespace generated component names and their references for one exported
/// app. Author-declared `#[schema(name = "...")]` names remain global.
pub fn namespace_export_schemas(
    openapi: &mut vespera_core::OpenApi,
    metadata: &CollectedMetadata,
    namespace: &str,
) -> syn::Result<()> {
    if namespace.is_empty() {
        return Ok(());
    }

    let explicit_names: HashSet<&str> = metadata
        .structs
        .iter()
        .filter(|schema| schema.explicit_schema_name)
        .map(|schema| schema.name.as_str())
        .collect();
    let mut value = serde_json::to_value(&*openapi).map_err(|error| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("export_app! macro: failed to namespace OpenAPI schemas: {error}"),
        )
    })?;
    let Some(schemas) = value
        .pointer_mut("/components/schemas")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };

    let mut renames = BTreeMap::new();
    for name in schemas
        .keys()
        .filter(|name| !explicit_names.contains(name.as_str()))
    {
        renames.insert(name.clone(), format!("{namespace}{name}"));
    }
    for (old_name, new_name) in &renames {
        if schemas.contains_key(new_name) && !renames.contains_key(new_name) && old_name != new_name
        {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "export_app! macro: schema namespace `{namespace}` maps `{old_name}` to existing component `{new_name}`"
                ),
            ));
        }
    }
    for (old_name, new_name) in &renames {
        if old_name != new_name
            && let Some(schema) = schemas.remove(old_name)
        {
            schemas.insert(new_name.clone(), schema);
        }
    }
    rewrite_schema_refs(&mut value, &renames);
    *openapi = serde_json::from_value(value).map_err(|error| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("export_app! macro: failed to rebuild namespaced OpenAPI spec: {error}"),
        )
    })?;
    Ok(())
}

fn rewrite_schema_refs(value: &mut serde_json::Value, renames: &BTreeMap<String, String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(reference)) = object.get_mut("$ref")
                && let Some(target) = reference.strip_prefix("#/components/schemas/")
            {
                let (name, suffix) = target
                    .split_once('/')
                    .map_or((target, ""), |(name, suffix)| (name, suffix));
                if let Some(new_name) = renames.get(name) {
                    *reference = if suffix.is_empty() {
                        format!("#/components/schemas/{new_name}")
                    } else {
                        format!("#/components/schemas/{new_name}/{suffix}")
                    };
                }
            }
            for child in object.values_mut() {
                rewrite_schema_refs(child, renames);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                rewrite_schema_refs(child, renames);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_app_input_name_only() {
        let tokens = quote::quote!(MyApp);
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert!(input.dir.is_none());
        assert!(input.prefix.is_none());
    }

    #[test]
    fn test_export_app_input_with_dir() {
        let tokens = quote::quote!(MyApp, dir = "api");
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert_eq!(input.dir.unwrap().value(), "api");
    }

    #[test]
    fn test_export_app_input_with_trailing_comma() {
        let tokens = quote::quote!(MyApp,);
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert!(input.dir.is_none());
    }

    #[test]
    fn test_export_app_input_unknown_field() {
        let tokens = quote::quote!(MyApp, unknown = "value");
        let result: syn::Result<ExportAppInput> = syn::parse2(tokens);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_compile_error().to_string().contains("unknown field"));
    }

    #[test]
    fn test_export_app_input_multiple_commas() {
        let tokens = quote::quote!(MyApp, dir = "api",);
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert_eq!(input.dir.unwrap().value(), "api");
    }

    #[test]
    fn test_export_app_input_duplicate_dir() {
        // A repeated `dir` must be a spanned compile error, not a silent
        // last-wins overwrite.
        let tokens = quote::quote!(MyApp, dir = "api", dir = "other");
        let result: syn::Result<ExportAppInput> = syn::parse2(tokens);
        assert!(result.is_err(), "duplicate `dir` must be rejected");
        assert!(
            result
                .err()
                .unwrap()
                .to_compile_error()
                .to_string()
                .contains("duplicate field `dir`")
        );
    }

    #[test]
    fn test_export_app_input_with_normalized_prefix() {
        let input: ExportAppInput =
            syn::parse2(quote::quote!(MyApp, prefix = "api/media-library/")).unwrap();

        assert_eq!(input.prefix.unwrap().value(), "/api/media-library");
    }

    #[test]
    fn test_export_app_input_duplicate_prefix() {
        let result: syn::Result<ExportAppInput> =
            syn::parse2(quote::quote!(MyApp, prefix = "/api", prefix = "/other"));

        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("duplicate field `prefix`")
        );
    }

    #[test]
    fn test_export_app_input_empty_and_root_prefix_are_noops() {
        for tokens in [
            quote::quote!(MyApp, prefix = ""),
            quote::quote!(MyApp, prefix = "/"),
        ] {
            let input: ExportAppInput = syn::parse2(tokens).unwrap();
            assert_eq!(input.prefix.unwrap().value(), "");
        }
    }

    #[test]
    fn test_export_app_input_rejects_invalid_prefix() {
        let result: syn::Result<ExportAppInput> =
            syn::parse2(quote::quote!(MyApp, prefix = "/api?version=1"));

        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("must be a URL path")
        );
    }

    #[rstest::rstest]
    #[case("/api media", "must be a URL path")]
    #[case("/api?version=1", "must be a URL path")]
    #[case("/api#section", "must be a URL path")]
    #[case("/api//users", "must not contain empty path segments")]
    #[case("/---", "must contain at least one alphanumeric character")]
    fn normalize_prefix_rejects_each_invalid_shape(#[case] raw: &str, #[case] expected: &str) {
        let prefix = LitStr::new(raw, proc_macro2::Span::call_site());

        let error = normalize_prefix(&prefix).expect_err("invalid prefix must be rejected");

        assert!(error.to_string().contains(expected));
    }

    #[rstest::rstest]
    #[case("", "")]
    #[case("/", "")]
    #[case("/api", "Api")]
    #[case("/api/media-library", "MediaLibrary")]
    #[case("/api/v1/user_profile", "V1UserProfile")]
    #[case("/api/-media--library-", "MediaLibrary")]
    fn schema_namespace_covers_empty_api_and_composite_prefixes(
        #[case] prefix: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(schema_namespace_from_prefix(prefix), expected);
    }

    fn route_metadata(path: &str) -> crate::metadata::RouteMetadata {
        crate::metadata::RouteMetadata {
            method: "get".to_string(),
            path: path.to_string(),
            function_name: "get_user".to_string(),
            module_path: "routes::users".to_string(),
            file_path: "users.rs".to_string(),
            success_status: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
        }
    }

    #[test]
    fn prefix_keeps_router_and_openapi_paths_identical_with_parameter() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_metadata("/users/{user_id}"));
        apply_export_prefix(&mut metadata, "/api/media-library");

        let router =
            crate::router_codegen::generate_router_code(&metadata, None, None, None, &[], &[])
                .to_string();
        let file_cache = std::collections::HashMap::from([(
            "users.rs".to_string(),
            syn::parse_file("pub async fn get_user() {}").unwrap(),
        )]);
        let openapi = crate::openapi_generator::generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            Some(file_cache),
            &[],
        );

        let expected = "/api/media-library/users/{user_id}";
        assert_eq!(metadata.routes[0].path, expected);
        assert!(router.contains(expected));
        assert!(openapi.paths.contains_key(expected));
    }

    #[test]
    fn no_prefix_leaves_route_metadata_unchanged() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_metadata("/users/{user_id}"));
        let before = serde_json::to_vec(&metadata).unwrap();

        apply_export_prefix(&mut metadata, "");

        assert_eq!(serde_json::to_vec(&metadata).unwrap(), before);
    }

    #[test]
    fn prefix_replaces_root_route_and_extends_nested_route() {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_metadata("/"));
        metadata.routes.push(route_metadata("/users"));

        apply_export_prefix(&mut metadata, "/api/admin");

        assert_eq!(metadata.routes[0].path, "/api/admin");
        assert_eq!(metadata.routes[1].path, "/api/admin/users");
    }

    #[test]
    fn nonempty_prefix_leaves_empty_route_collection_empty() {
        let mut metadata = CollectedMetadata::new();

        apply_export_prefix(&mut metadata, "/api/admin");

        assert!(metadata.routes.is_empty());
    }

    fn schema_metadata(
        name: &str,
        definition: &str,
        explicit_schema_name: bool,
    ) -> crate::metadata::StructMetadata {
        crate::metadata::StructMetadata {
            name: name.to_string(),
            definition: definition.to_string(),
            explicit_schema_name,
            ..Default::default()
        }
    }

    fn schema_doc(path: &str, item_definition: &str) -> (CollectedMetadata, vespera_core::OpenApi) {
        let mut metadata = CollectedMetadata::new();
        metadata.routes.push(route_metadata(path));
        metadata
            .structs
            .push(schema_metadata("Item", item_definition, false));
        metadata.structs.push(schema_metadata(
            "SharedThing",
            "struct SharedThing { code: String }",
            true,
        ));
        let file_cache = std::collections::HashMap::from([(
            "users.rs".to_string(),
            syn::parse_file("pub async fn get_user() -> Item { todo!() }").unwrap(),
        )]);
        let openapi = crate::openapi_generator::generate_openapi_doc_with_metadata(
            None,
            None,
            None,
            None,
            &metadata,
            Some(file_cache),
            &[],
        );
        (metadata, openapi)
    }

    fn schema_refs(value: &serde_json::Value, refs: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(serde_json::Value::String(reference)) = object.get("$ref") {
                    refs.push(reference.clone());
                }
                for child in object.values() {
                    schema_refs(child, refs);
                }
            }
            serde_json::Value::Array(values) => {
                for child in values {
                    schema_refs(child, refs);
                }
            }
            _ => {}
        }
    }

    fn assert_all_component_refs_resolve(openapi: &vespera_core::OpenApi) {
        let value = serde_json::to_value(openapi).unwrap();
        let schemas = value
            .pointer("/components/schemas")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        let mut refs = Vec::new();
        schema_refs(&value, &mut refs);
        for reference in refs {
            if let Some(target) = reference.strip_prefix("#/components/schemas/") {
                let name = target.split('/').next().unwrap();
                assert!(
                    schemas.contains_key(name),
                    "dangling schema ref: {reference}"
                );
            }
        }
    }

    #[test]
    fn prefix_namespaces_generated_schemas_and_preserves_explicit_names() {
        let (metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");

        let namespace = schema_namespace_from_prefix("/api/media-library");
        namespace_export_schemas(&mut openapi, &metadata, &namespace).unwrap();

        let schemas = openapi
            .components
            .as_ref()
            .and_then(|components| components.schemas.as_ref())
            .unwrap();
        assert_eq!(namespace, "MediaLibrary");
        assert!(schemas.contains_key("MediaLibraryItem"));
        assert!(!schemas.contains_key("Item"));
        assert!(schemas.contains_key("SharedThing"));
        assert_all_component_refs_resolve(&openapi);
        let json = serde_json::to_string(&openapi).unwrap();
        assert!(json.contains("#/components/schemas/MediaLibraryItem"));
    }

    #[test]
    fn differently_prefixed_apps_merge_distinct_schemas_and_refs() {
        let (first_metadata, mut first) =
            schema_doc("/api/media/items", "struct Item { media_id: i32 }");
        namespace_export_schemas(&mut first, &first_metadata, "Media").unwrap();
        let (second_metadata, mut second) =
            schema_doc("/api/catalog/items", "struct Item { sku: String }");
        namespace_export_schemas(&mut second, &second_metadata, "Catalog").unwrap();

        first.merge(second);

        let schemas = first
            .components
            .as_ref()
            .and_then(|components| components.schemas.as_ref())
            .unwrap();
        assert!(schemas.contains_key("MediaItem"));
        assert!(schemas.contains_key("CatalogItem"));
        assert_all_component_refs_resolve(&first);
        let json = serde_json::to_string(&first).unwrap();
        assert!(json.contains("#/components/schemas/MediaItem"));
        assert!(json.contains("#/components/schemas/CatalogItem"));
    }

    #[test]
    fn empty_namespace_leaves_components_and_refs_byte_identical() {
        let (metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");
        let before = serde_json::to_vec(&openapi).unwrap();

        namespace_export_schemas(&mut openapi, &metadata, "").unwrap();

        assert_eq!(serde_json::to_vec(&openapi).unwrap(), before);
    }
}
