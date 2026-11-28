use std::collections::{BTreeMap, HashSet};

use syn::{
    LitStr,
    parse::{Parse, ParseStream},
};

use vespera_core::schema::{escape_schema_name, split_schema_ref};

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
    // Checked before the trailing slashes go: `//` and `/api//` are empty
    // segments, not a prefix that merely ends in a slash.
    if with_leading_slash.contains("//") {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must not contain empty path segments (`//`)",
        ));
    }
    let normalized = with_leading_slash.trim_end_matches('/');
    // `.` / `..` (also percent-encoded) and `\` are resolved by clients, so the
    // routes would be requested under another path than the one registered.
    if super::path_segments::resolved_by_clients(normalized) {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must not contain `.` or `..` path segments (also percent-encoded) or a backslash (clients resolve them)",
        ));
    }
    if normalized.contains([':', '*', '{', '}']) {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must be a static path: `:`, `*`, `{` and `}` are axum route-parameter syntax",
        ));
    }
    if !super::path_segments::is_ascii_path(normalized) {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must be ASCII URL path characters (percent-encode any other byte)",
        ));
    }
    if !normalized.is_empty() && schema_namespace_from_prefix(normalized).is_empty() {
        return Err(syn::Error::new(
            prefix.span(),
            "export_app! macro: `prefix` must contain at least one ASCII alphanumeric character (it names the app's components)",
        ));
    }

    Ok(LitStr::new(normalized, prefix.span()))
}

/// Derive a deterministic PascalCase component namespace from a normalized
/// route prefix. A conventional leading `/api` segment is omitted when more
/// specific segments follow (`/api/media-library` -> `MediaLibrary`) and
/// name something (`/api/_` keeps `Api`).  Only ASCII alphanumerics are
/// kept: an OpenAPI component key must match `^[a-zA-Z0-9.\-_]+$`.
pub fn schema_namespace_from_prefix(prefix: &str) -> String {
    let segments: Vec<&str> = prefix
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let start = usize::from(segments.len() > 1 && segments[0].eq_ignore_ascii_case("api"));
    let namespace = pascal_namespace(&segments[start..]);
    if namespace.is_empty() {
        pascal_namespace(&segments)
    } else {
        namespace
    }
}

/// The PascalCase words of `segments` (ASCII alphanumeric runs).
fn pascal_namespace(segments: &[&str]) -> String {
    let mut namespace = String::new();
    for word in segments
        .iter()
        .flat_map(|segment| segment.split(|ch: char| !ch.is_ascii_alphanumeric()))
    {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            namespace.push(first.to_ascii_uppercase());
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

    let prefixed = |path: &str| {
        if path == "/" {
            prefix.to_owned()
        } else {
            format!("{prefix}{path}")
        }
    };
    for route in &mut metadata.routes {
        route.path = prefixed(&route.path);
    }
    // `#[cfg]`-gated routes are mounted by the same router.
    for route in &mut metadata.cfg_routes {
        route.path = prefixed(&route.path);
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
    // Round-trip through JSON text rather than `to_value`/`from_value`: the
    // text serializer/reader are already instantiated for the spec, whereas the
    // `Value` (de)serializer would compile a second copy of every OpenAPI type's
    // serde impl into the proc-macro.
    let mut value = serde_json::to_string(&*openapi)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text))
        .map_err(|error| {
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
    // Remove every renamed entry before inserting any: a chained rename
    // (`User` -> `V1User` while a real `V1User` -> `V1V1User`) must not let
    // one insert clobber a component that is renamed later.
    let moved: Vec<(&String, serde_json::Value)> = renames
        .iter()
        .filter_map(|(old_name, new_name)| {
            schemas.remove(old_name).map(|schema| (new_name, schema))
        })
        .collect();
    for (new_name, schema) in moved {
        schemas.insert(new_name.clone(), schema);
    }
    rewrite_document_refs(&mut value, &renames);
    *openapi = serde_json::to_string(&value)
        .and_then(|text| serde_json::from_str(&text))
        .map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("export_app! macro: failed to rebuild namespaced OpenAPI spec: {error}"),
            )
        })?;
    Ok(())
}

/// Keys whose values are data (example payloads, defaults, enum members), not
/// schemas: a `$ref`-shaped value under them is documented content.
const DATA_KEYS: [&str; 5] = ["example", "examples", "default", "enum", "const"];

/// Keys whose object maps names (property names, component names, status
/// codes, paths) to schemas or objects: its own keys are never data keywords,
/// so a property named `example` or a `default` response is still walked.
const NAME_MAP_KEYS: [&str; 15] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "schemas",
    "responses",
    "paths",
    // The other component maps: a component named `default` / `example` /
    // `enum` is a name, not data.
    "parameters",
    "requestBodies",
    "headers",
    "callbacks",
    "links",
    "pathItems",
    // The root `webhooks` map (OpenAPI 3.1) is keyed by webhook name, so a
    // webhook named `default` / `example` is a name, not data.
    "webhooks",
];

/// Point every `#/components/schemas/{old}` `$ref` in a schema position inside
/// `value` at `renames[old]`. `value` is a component map or a `paths` object
/// (its keys are names, not keywords); a whole OpenAPI document goes through
/// [`rewrite_document_refs`], since its root is a keyword object (`webhooks`
/// would otherwise lose its names). Example / default / enum data is left as
/// written.
pub fn rewrite_schema_refs(value: &mut serde_json::Value, renames: &BTreeMap<String, String>) {
    rewrite_refs(value, renames, true);
}

/// [`rewrite_schema_refs`] for a whole OpenAPI document. Its root is an
/// object of keywords, so `webhooks` (a name map, see [`NAME_MAP_KEYS`]) is
/// reached as a name map here; a bare component map passed to
/// [`rewrite_schema_refs`] has names as its own keys instead.
fn rewrite_document_refs(document: &mut serde_json::Value, renames: &BTreeMap<String, String>) {
    rewrite_refs(document, renames, false);
}

/// [`rewrite_schema_refs`]; `names` says `value` is a name map (its keys are
/// names, not keywords). The root is one; below it, only the NAME_MAP_KEYS
/// values of a schema-level object are (a root-level `paths` is walked as a
/// document object).
fn rewrite_refs(value: &mut serde_json::Value, renames: &BTreeMap<String, String>, names: bool) {
    match value {
        serde_json::Value::Object(object) => {
            // RFC 6901: the fragment is percent-decoded before it is split into
            // tokens, so `Pet%2Fx` names `Pet` with the suffix `%2Fx`; the
            // suffix is kept as written.
            if !names
                && let Some(serde_json::Value::String(reference)) = object.get_mut("$ref")
                && let Some((name, suffix)) = split_schema_ref(reference)
                && let Some(new_name) = renames.get(&name)
            {
                *reference = format!(
                    "#/components/schemas/{}{suffix}",
                    escape_schema_name(new_name)
                );
            }
            for (key, child) in object.iter_mut() {
                if !names && DATA_KEYS.contains(&key.as_str()) {
                    continue;
                }
                let child_names = !names && NAME_MAP_KEYS.contains(&key.as_str());
                rewrite_refs(child, renames, child_names);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                rewrite_refs(child, renames, false);
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
    #[case("//", "must not contain empty path segments")]
    #[case("/api//", "must not contain empty path segments")]
    #[case("/api/../v1", "must not contain `.` or `..` path segments")]
    #[case("/./api", "must not contain `.` or `..` path segments")]
    #[case("/api/..", "must not contain `.` or `..` path segments")]
    #[case("/api/%2E%2e/admin", "must not contain `.` or `..` path segments")]
    #[case("/a/.%2e", "must not contain `.` or `..` path segments")]
    #[case("/api\\..\\x", "must not contain `.` or `..` path segments")]
    #[case("/api\\users", "must not contain `.` or `..` path segments")]
    #[case("/---", "must contain at least one ASCII alphanumeric character")]
    #[case("/카탈로그", "must be ASCII URL path characters")]
    #[case("/api%zz", "must be ASCII URL path characters")]
    #[case("/api/{tenant}", "must be a static path")]
    #[case("/api/*rest", "must be a static path")]
    #[case("/api/:id", "must be a static path")]
    #[case("/api}", "must be a static path")]
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
    #[case("/catalog-v2/ete%C3%A9", "CatalogV2EteC3A9")]
    #[case("/api/_", "Api")]
    #[case("/api/-/--", "Api")]
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

        let router = crate::router_codegen::generate_router_code(
            &metadata,
            None,
            None,
            None,
            None,
            &[],
            &[],
        )
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

    /// `#[cfg]`-gated routes are mounted by the same router, so they get the
    /// prefix too.
    #[test]
    fn prefix_applies_to_cfg_gated_routes() {
        let gated = |path: &str| crate::metadata::CfgRoute {
            cfg: vec!["feature = \"admin\"".to_string()],
            method: "get".to_string(),
            path: path.to_string(),
            module_path: "routes::admin".to_string(),
            function_name: "list".to_string(),
            file_path: "admin.rs".to_string(),
            deprecated: false,
        };
        let mut metadata = CollectedMetadata::new();
        metadata.cfg_routes.push(gated("/"));
        metadata.cfg_routes.push(gated("/admin"));

        apply_export_prefix(&mut metadata, "/api");

        let paths: Vec<&str> = metadata
            .cfg_routes
            .iter()
            .map(|r| r.path.as_str())
            .collect();
        assert_eq!(paths, ["/api", "/api/admin"]);
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

    /// A `$ref`-shaped value that is documented data (an `example`, a schema's
    /// `default`, an `enum` member) is not a schema reference; a property or a
    /// response named like a keyword still is.
    #[test]
    fn rewrite_leaves_example_and_default_data_alone() {
        let mut value = serde_json::json!({
            "components": { "schemas": {
                "Holder": {
                    "properties": {
                        "example": { "$ref": "#/components/schemas/Item" },
                        "default": { "$ref": "#/components/schemas/Item" }
                    },
                    "default": { "$ref": "#/components/schemas/Item" },
                    "example": { "$ref": "#/components/schemas/Item" }
                },
                "default": { "$ref": "#/components/schemas/Item" },
                "Item": { "type": "object" }
            } },
            "paths": { "/x": { "get": { "responses": {
                "default": { "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/Item" },
                    "example": { "$ref": "#/components/schemas/Item" }
                } } }
            } } } }
        });
        let renames = BTreeMap::from([("Item".to_string(), "MediaItem".to_string())]);

        rewrite_schema_refs(&mut value, &renames);

        let holder = &value["components"]["schemas"]["Holder"];
        assert_eq!(
            holder["properties"]["example"]["$ref"],
            "#/components/schemas/MediaItem"
        );
        assert_eq!(
            holder["properties"]["default"]["$ref"],
            "#/components/schemas/MediaItem"
        );
        assert_eq!(holder["default"]["$ref"], "#/components/schemas/Item");
        assert_eq!(holder["example"]["$ref"], "#/components/schemas/Item");
        // A component named `default` is a schema, not data.
        assert_eq!(
            value["components"]["schemas"]["default"]["$ref"],
            "#/components/schemas/MediaItem"
        );
        let media =
            &value["paths"]["/x"]["get"]["responses"]["default"]["content"]["application/json"];
        assert_eq!(media["schema"]["$ref"], "#/components/schemas/MediaItem");
        assert_eq!(media["example"]["$ref"], "#/components/schemas/Item");
    }

    /// A component named like a data keyword (`default`, `example`, `enum`,
    /// ...) is namespaced in every component map, not only `schemas`.
    #[test]
    fn component_names_in_every_component_map_are_namespaced() {
        let item = |name: &str| serde_json::json!({ name: { "schema": { "$ref": "#/components/schemas/Item" } } });
        let mut value = serde_json::json!({
            "components": {
                "schemas": { "Item": { "type": "object" } },
                "parameters": item("default"),
                "requestBodies": item("example"),
                "headers": item("enum"),
                "callbacks": { "const": { "{$request.body#/url}": { "post": {
                    "requestBody": { "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/Item" }
                    } } }
                } } } },
                "links": item("examples"),
                "pathItems": item("default")
            }
        });
        let renames = BTreeMap::from([("Item".to_string(), "MediaItem".to_string())]);

        rewrite_schema_refs(&mut value, &renames);

        let media = "#/components/schemas/MediaItem";
        let components = &value["components"];
        assert_eq!(components["parameters"]["default"]["schema"]["$ref"], media);
        assert_eq!(
            components["requestBodies"]["example"]["schema"]["$ref"],
            media
        );
        assert_eq!(components["headers"]["enum"]["schema"]["$ref"], media);
        assert_eq!(components["links"]["examples"]["schema"]["$ref"], media);
        assert_eq!(components["pathItems"]["default"]["schema"]["$ref"], media);
        assert_eq!(
            components["callbacks"]["const"]["{$request.body#/url}"]["post"]["requestBody"]["content"]
                ["application/json"]["schema"]["$ref"],
            media,
            "{value:#}"
        );
    }

    /// RFC 6901 decodes the `$ref` fragment before it is split into tokens:
    /// `Pet%2Fx` names the component `Pet` with the suffix `%2Fx`, so a renamed
    /// `Pet` follows it, and the raw suffix is kept as written.
    #[test]
    fn encoded_slash_in_a_ref_names_the_component_before_it() {
        let mut value = serde_json::json!({
            "components": { "schemas": {
                "Holder": { "properties": {
                    "a": { "$ref": "#/components/schemas/Pet%2Fx" },
                    "b": { "$ref": "#/components/schemas/Pet%2Fx/properties/id" },
                    "c": { "$ref": "#/components/schemas/Pet" }
                } },
                "Pet": { "type": "object" }
            } }
        });
        let renames = BTreeMap::from([("Pet".to_string(), "Animal".to_string())]);

        rewrite_schema_refs(&mut value, &renames);

        let props = &value["components"]["schemas"]["Holder"]["properties"];
        assert_eq!(props["a"]["$ref"], "#/components/schemas/Animal%2Fx");
        assert_eq!(
            props["b"]["$ref"],
            "#/components/schemas/Animal%2Fx/properties/id"
        );
        assert_eq!(props["c"]["$ref"], "#/components/schemas/Animal");
    }

    /// A root `webhooks` entry named like a data keyword (`default`) is a
    /// webhook name: its schema `$ref` follows the namespace like a path's.
    #[test]
    fn root_webhook_names_are_namespaced() {
        let (metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");
        openapi.webhooks = serde_json::from_value(serde_json::json!({
            "default": { "post": { "requestBody": { "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Item" }
            } } } } }
        }))
        .unwrap();

        namespace_export_schemas(&mut openapi, &metadata, "Media").unwrap();

        let webhooks = serde_json::to_value(&openapi.webhooks).unwrap();
        assert_eq!(
            webhooks["default"]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/MediaItem",
            "{webhooks:#}"
        );
    }

    #[test]
    fn empty_namespace_leaves_components_and_refs_byte_identical() {
        let (metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");
        let before = serde_json::to_vec(&openapi).unwrap();

        namespace_export_schemas(&mut openapi, &metadata, "").unwrap();

        assert_eq!(serde_json::to_vec(&openapi).unwrap(), before);
    }

    #[test]
    fn nonempty_namespace_without_schema_components_is_a_noop() {
        let (metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");
        openapi.components = None;
        let before = serde_json::to_vec(&openapi).unwrap();

        namespace_export_schemas(&mut openapi, &metadata, "Media").unwrap();

        assert_eq!(serde_json::to_vec(&openapi).unwrap(), before);
    }

    /// A generated `X` and a generated `<Ns>X` both get namespaced; the
    /// chained rename (`User` -> `V1User`, `V1User` -> `V1V1User`) must keep
    /// both bodies instead of letting the first insert clobber the second.
    #[test]
    fn chained_namespace_renames_keep_every_schema_body() {
        let (metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");
        let schemas = openapi
            .components
            .as_mut()
            .and_then(|components| components.schemas.as_mut())
            .unwrap();
        let item = schemas["Item"].clone();
        let mut marker = item.clone();
        marker.description = Some("real V1Item".to_string());
        schemas.insert("V1Item".to_string(), marker);

        namespace_export_schemas(&mut openapi, &metadata, "V1").unwrap();

        let schemas = openapi
            .components
            .as_ref()
            .and_then(|components| components.schemas.as_ref())
            .unwrap();
        assert_eq!(schemas["V1Item"].description, item.description);
        assert_eq!(
            schemas["V1V1Item"].description.as_deref(),
            Some("real V1Item")
        );
        assert!(!schemas.contains_key("Item"));
        assert_all_component_refs_resolve(&openapi);
    }

    #[test]
    fn generated_schema_namespace_rejects_an_explicit_name_collision() {
        let (mut metadata, mut openapi) = schema_doc("/items", "struct Item { id: i32 }");
        metadata.structs.push(schema_metadata(
            "MediaItem",
            "struct MediaItem { id: i32 }",
            true,
        ));
        let schemas = openapi
            .components
            .as_mut()
            .and_then(|components| components.schemas.as_mut())
            .unwrap();
        schemas.insert("MediaItem".to_string(), schemas["Item"].clone());

        let error = namespace_export_schemas(&mut openapi, &metadata, "Media")
            .expect_err("generated names must not replace explicit schemas");

        assert!(
            error
                .to_string()
                .contains("schema namespace `Media` maps `Item` to existing component `MediaItem`")
        );
    }
}
