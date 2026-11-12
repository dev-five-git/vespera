//! Component schema lookup, file-cache indexing, and schema parsing.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

use crate::{
    metadata::CollectedMetadata,
    openapi_generator::{defaults::process_default_functions, paths::parallel_filter_map},
    parser::{parse_enum_to_schema, parse_struct_to_schema},
};

/// Build schema name and definition lookup maps from metadata.
///
/// Registers ALL structs (including `include_in_openapi: false`) so that
/// `schema_type!` generated types can reference them.
pub(super) fn build_schema_lookups(
    metadata: &CollectedMetadata,
) -> (HashSet<&str>, HashMap<&str, &str>) {
    let mut known_schema_names = HashSet::with_capacity(metadata.structs.len());
    let mut struct_definitions = HashMap::with_capacity(metadata.structs.len());

    for struct_meta in &metadata.structs {
        struct_definitions.insert(struct_meta.name.as_str(), struct_meta.definition.as_str());
        known_schema_names.insert(struct_meta.name.as_str());
    }
    // `#[schema(name = "X")] struct Foo` is registered as `X`, but Rust code
    // names it `Foo`: alias the Rust ident onto the same definition so every
    // use resolves (the type conversion then emits `$ref: X`, see
    // `component_name` in `parser::schema::type_schema`). A real schema of
    // the same name always wins over an alias.
    for struct_meta in &metadata.structs {
        if let Some(ident) = struct_meta.rust_ident.as_deref()
            && !struct_definitions.contains_key(ident)
        {
            struct_definitions.insert(ident, struct_meta.definition.as_str());
            known_schema_names.insert(ident);
        }
    }

    (known_schema_names, struct_definitions)
}

/// Build file AST cache — parse each unique route file exactly once.
///
/// Deduplicates file paths first, then parses each file a single time.
/// This eliminates redundant file I/O when multiple routes share a source file.
pub(super) fn build_file_cache(metadata: &CollectedMetadata) -> HashMap<String, syn::File> {
    let unique_paths: BTreeSet<&str> = metadata
        .routes
        .iter()
        .map(|r| r.file_path.as_str())
        .collect();
    let mut cache = HashMap::with_capacity(unique_paths.len());
    for path in unique_paths {
        if let Some(ast) = crate::schema_macro::file_cache::get_parsed_file(Path::new(path)) {
            cache.insert(path.to_string(), ast);
        }
    }
    cache
}

/// Build struct name → file path index from cached file ASTs.
///
/// Enables O(1) lookup of which file contains a given struct definition,
/// replacing the previous O(routes × file_read) linear scan.
pub(super) fn build_struct_file_index(
    file_cache: &HashMap<String, syn::File>,
) -> HashMap<String, &str> {
    let mut index = HashMap::with_capacity(file_cache.len() * 4);
    for (path, ast) in file_cache {
        for item in &ast.items {
            if let syn::Item::Struct(s) = item {
                index.insert(s.ident.to_string(), path.as_str());
            }
        }
    }
    index
}

/// Parse struct and enum definitions into `OpenAPI` component schemas.
///
/// Only includes structs where `include_in_openapi` is true
/// (i.e., from `#[derive(Schema)]`, not from cross-file lookup).
/// Also processes `#[serde(default)]` attributes to extract default values.
///
/// Uses pre-built `file_cache` and `struct_file_index` for O(1) file lookups
/// instead of scanning all route files per struct.
pub(super) fn parse_component_schemas(
    metadata: &CollectedMetadata,
    known_schema_names: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
    file_cache: &HashMap<String, syn::File>,
    struct_file_index: &HashMap<String, &str>,
) -> syn::Result<BTreeMap<String, vespera_core::schema::Schema>> {
    // Parse a definition string and build its schema, applying the
    // default-value pipeline.  `file_ast` is only needed for the
    // `#[serde(default = "fn_name")]` fallback (Priority 2) — the
    // pre-extracted SCHEMA_STORAGE defaults, `#[schema(default)]`
    // attributes, and type defaults apply even without an AST (the
    // collector fast path skips parsing, leaving `file_cache` empty).
    let build_one = |struct_meta: &crate::metadata::StructMetadata,
                     file_ast: Option<&syn::File>|
     -> syn::Result<Option<(String, vespera_core::schema::Schema)>> {
        let parsed = syn::parse_str::<syn::Item>(&struct_meta.definition).map_err(|err| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "failed to parse component schema `{}` metadata: {err}",
                    struct_meta.name
                ),
            )
        })?;
        let mut schema = match &parsed {
            syn::Item::Struct(struct_item) => {
                parse_struct_to_schema(struct_item, known_schema_names, struct_definitions)
            }
            syn::Item::Enum(enum_item) => {
                parse_enum_to_schema(enum_item, known_schema_names, struct_definitions)
            }
            _ => return Ok(None),
        };
        if let syn::Item::Struct(struct_item) = &parsed {
            process_default_functions(
                struct_item,
                file_ast,
                &mut schema,
                &struct_meta.field_defaults,
            );
        }
        Ok(Some((struct_meta.name.clone(), schema)))
    };

    // Partition: structs whose file AST is reachable need the
    // (non-`Send`) AST for Priority-2 default extraction and run on
    // this thread; everything else parses + builds on workers
    // returning plain `Schema` data.
    let mut ast_backed: Vec<(&crate::metadata::StructMetadata, &syn::File)> = Vec::new();
    let mut parallel_jobs: Vec<&crate::metadata::StructMetadata> = Vec::new();
    for struct_meta in metadata.structs.iter().filter(|s| s.include_in_openapi) {
        // Use ONLY the struct's own indexed file AST for Priority-2
        // (`#[serde(default = "fn")]`) default extraction.  The former
        // fallback to `metadata.routes.first()`'s AST could resolve a
        // same-named default fn from an UNRELATED route file and emit a
        // wrong OpenAPI default; a struct whose file is not indexed now
        // simply forgoes Priority-2 extraction (other default sources still
        // apply) rather than risking an incorrect value.
        let file_ast = struct_file_index
            .get(&struct_meta.name)
            .or_else(|| {
                struct_meta
                    .rust_ident
                    .as_ref()
                    .and_then(|ident| struct_file_index.get(ident))
            })
            .and_then(|path| file_cache.get(*path));
        match file_ast {
            Some(ast) => ast_backed.push((struct_meta, ast)),
            None => parallel_jobs.push(struct_meta),
        }
    }

    let mut schemas = BTreeMap::new();
    for (name, schema) in parallel_filter_map(
        &parallel_jobs,
        &|meta: &&crate::metadata::StructMetadata| build_one(meta, None),
    )? {
        schemas.insert(name, schema);
    }
    for (struct_meta, ast) in ast_backed {
        if let Some((name, schema)) = build_one(struct_meta, Some(ast))? {
            schemas.insert(name, schema);
        }
    }

    reject_internally_tagged_non_object_refs(&schemas)?;
    Ok(schemas)
}

/// Reject an internally tagged enum newtype variant that wraps a `$ref` to a
/// component known to serialize as a non-object (e.g. `struct UserId(String)`
/// emitting a `string` component).  `#[derive(Schema)]` only sees the variant's
/// type NAME, so this can only be decided once every component is built; a
/// `$ref` to a component not in this spec (external / merged) is left alone.
///
/// Internally tagged newtype variants are the only `oneOf` branches emitted as
/// `allOf: [{tag object}, {$ref}]` under a `discriminator`.
fn reject_internally_tagged_non_object_refs(
    schemas: &BTreeMap<String, vespera_core::schema::Schema>,
) -> syn::Result<()> {
    use vespera_core::schema::{SchemaRef, SchemaType};
    for (enum_name, schema) in schemas {
        let Some(discriminator) = &schema.discriminator else {
            continue;
        };
        let tag = &discriminator.property_name;
        for branch in schema.one_of.iter().flatten() {
            let SchemaRef::Inline(branch) = branch else {
                continue;
            };
            let Some([SchemaRef::Inline(tag_object), SchemaRef::Ref(reference)]) =
                branch.all_of.as_deref()
            else {
                continue;
            };
            let Some(target) = reference.ref_path.strip_prefix("#/components/schemas/") else {
                continue;
            };
            let non_object = schemas.get(target).is_some_and(|target| {
                target
                    .schema_type
                    .is_some_and(|ty| !matches!(ty, SchemaType::Object | SchemaType::Null))
            });
            if !non_object {
                continue;
            }
            let variant = tag_object
                .properties
                .as_ref()
                .and_then(|properties| properties.get(tag))
                .and_then(|tag_schema| match tag_schema {
                    SchemaRef::Inline(tag_schema) => tag_schema.r#enum.as_ref()?.first()?.as_str(),
                    SchemaRef::Ref(_) => None,
                })
                .unwrap_or("?");
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "`{enum_name}`: {}",
                    crate::parser::schema::internally_tagged_non_object_message(
                        tag, variant, target
                    )
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use rstest::rstest;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use vespera_core::schema::SchemaRef;

    use super::*;
    use crate::{
        metadata::{CollectedMetadata, RouteMetadata, StructMetadata},
        openapi_generator::{
            generate_openapi_doc_with_metadata, try_generate_openapi_doc_with_metadata,
        },
    };

    fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.path().join(filename);
        fs::write(&file_path, content).expect("Failed to write temp file");
        file_path
    }

    fn route_meta(path: &str, fn_name: &str, file_path: &str) -> RouteMetadata {
        RouteMetadata {
            method: "GET".to_string(),
            path: path.to_string(),
            function_name: fn_name.to_string(),
            module_path: format!("test::{fn_name}"),
            file_path: file_path.to_string(),
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
        }
    }

    fn struct_meta(name: &str, definition: &str) -> StructMetadata {
        StructMetadata {
            name: name.to_string(),
            definition: definition.to_string(),
            ..Default::default()
        }
    }

    fn schemas(
        doc: &vespera_core::openapi::OpenApi,
    ) -> &BTreeMap<String, vespera_core::schema::Schema> {
        doc.components
            .as_ref()
            .and_then(|c| c.schemas.as_ref())
            .expect("schemas present")
    }

    fn property_default<'a>(
        schema: &'a vespera_core::schema::Schema,
        field_name: &str,
    ) -> Option<&'a Value> {
        let SchemaRef::Inline(prop_schema) = schema.properties.as_ref()?.get(field_name)? else {
            return None;
        };
        prop_schema.default.as_ref()
    }

    #[test]
    fn schema_lookups_include_hidden_structs_for_references() {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Hidden".to_string(),
            definition: "struct Hidden { id: i32 }".to_string(),
            include_in_openapi: false,
            explicit_schema_name: false,
            field_defaults: BTreeMap::new(),
            source_identity: None,
            rust_ident: None,
        });

        let (known_schema_names, struct_definitions) = build_schema_lookups(&metadata);

        assert!(known_schema_names.contains("Hidden"));
        assert_eq!(
            *struct_definitions.get("Hidden").unwrap(),
            "struct Hidden { id: i32 }"
        );
    }

    /// An internally tagged newtype variant over a `$ref` to a component that
    /// is known to be a non-object (`struct UserId(String)` → `string`) is a
    /// compile error; an object component or an unknown `$ref` is accepted.
    #[rstest]
    #[case::string_newtype_component("string", "struct Inner(String);", true)]
    #[case::object_component("object", "struct Inner { id: i32 }", false)]
    #[case::unknown_component("unknown", "struct Unrelated { id: i32 }", false)]
    fn internally_tagged_newtype_over_non_object_ref(
        #[case] name: &str,
        #[case] inner_definition: &str,
        #[case] is_error: bool,
    ) {
        let mut metadata = CollectedMetadata::new();
        let inner_name = if name == "unknown" {
            "Unrelated"
        } else {
            "Inner"
        };
        metadata
            .structs
            .push(struct_meta(inner_name, inner_definition));
        metadata.structs.push(struct_meta(
            "Event",
            r#"#[serde(tag = "type")] enum Event { Ping, Data(Inner) }"#,
        ));
        let result =
            try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
        assert_eq!(result.is_err(), is_error, "{name}");
        if let Err(error) = result {
            insta::assert_snapshot!(
                format!("internally_tagged_newtype_ref_error_{name}"),
                error.to_string()
            );
        }
    }

    /// The post-build check only fires on the exact
    /// `allOf: [{tag object}, {$ref: "#/components/schemas/<non-object>"}]`
    /// shape; any other discriminated `oneOf` branch is left alone, and a tag
    /// schema it cannot read is reported as variant `?` rather than skipped.
    #[rstest]
    #[case::ref_branch(json!({ "$ref": "#/components/schemas/Inner" }), None)]
    #[case::external_ref(
        json!({ "allOf": [
            { "type": "object", "properties": { "type": { "type": "string", "enum": ["Data"] } } },
            { "$ref": "other.json#/Inner" }
        ] }),
        None
    )]
    #[case::ref_tag_schema(
        json!({ "allOf": [
            { "type": "object", "properties": { "type": { "$ref": "#/components/schemas/Tag" } } },
            { "$ref": "#/components/schemas/Inner" }
        ] }),
        Some("`?`")
    )]
    fn internally_tagged_check_inspects_only_tag_object_plus_local_ref(
        #[case] branch: Value,
        #[case] error_variant: Option<&str>,
    ) {
        let schemas: BTreeMap<String, vespera_core::schema::Schema> =
            serde_json::from_value(json!({
                "Inner": { "type": "string" },
                "Event": {
                    "discriminator": { "propertyName": "type" },
                    "oneOf": [branch]
                }
            }))
            .expect("hand-built components deserialize");
        match (
            reject_internally_tagged_non_object_refs(&schemas),
            error_variant,
        ) {
            (Ok(()), None) => {}
            (Err(error), Some(variant)) => {
                let message = error.to_string();
                assert!(message.starts_with("`Event`: "), "{message}");
                assert!(message.contains(variant), "{message}");
            }
            (got, want) => panic!("got {got:?}, want error variant {want:?}"),
        }
    }

    #[rstest]
    #[case::struct_schema("User", "struct User { id: i32, name: String }")]
    #[case::enum_schema("Status", "enum Status { Active, Inactive, Pending }")]
    #[case::enum_with_data(
        "Message",
        "enum Message { Text(String), User { id: i32, name: String } }"
    )]
    fn valid_component_definitions_are_included(#[case] name: &str, #[case] definition: &str) {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(struct_meta(name, definition));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert!(schemas(&doc).contains_key(name));
    }

    #[test]
    fn non_struct_non_enum_component_definitions_are_skipped() {
        let mut metadata = CollectedMetadata::new();
        metadata
            .structs
            .push(struct_meta("Config", "const CONFIG: i32 = 42;"));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert!(doc.components.is_none() || doc.components.as_ref().unwrap().schemas.is_none());
    }

    #[test]
    fn unparseable_component_definition_surfaces_error() {
        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Invalid".to_string(),
            definition: "struct { invalid syntax {{{{".to_string(),
            include_in_openapi: true,
            explicit_schema_name: false,
            field_defaults: BTreeMap::new(),
            source_identity: None,
            rust_ident: None,
        });

        let err =
            try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[])
                .expect_err("invalid component metadata must surface as an error");

        assert!(
            err.to_string()
                .contains("failed to parse component schema `Invalid` metadata"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn enum_schema_and_route_are_generated_together() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_file = create_temp_file(
            &temp_dir,
            "status_route.rs",
            "pub fn get_status() -> Status { Status::Active }",
        );

        let mut metadata = CollectedMetadata::new();
        metadata
            .structs
            .push(struct_meta("Status", "enum Status { Active, Inactive }"));
        metadata.routes.push(route_meta(
            "/status",
            "get_status",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

        assert!(schemas(&doc).contains_key("Status"));
        assert!(doc.paths.contains_key("/status"));
    }

    #[test]
    fn serde_default_function_sets_property_default() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_file = create_temp_file(
            &temp_dir,
            "user.rs",
            r#"
fn default_name() -> String { "John".to_string() }

struct User {
    #[serde(default = "default_name")]
    name: String,
}

pub fn get_user() -> User { User { name: "Alice".to_string() } }
"#,
        );

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(struct_meta(
            "User",
            r#"struct User { #[serde(default = "default_name")] name: String }"#,
        ));
        metadata.routes.push(route_meta(
            "/user",
            "get_user",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
        let user_schema = schemas(&doc).get("User").expect("User schema");

        assert_eq!(property_default(user_schema, "name"), Some(&json!("John")));
    }

    #[test]
    fn serde_simple_default_uses_type_defaults() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_file = create_temp_file(
            &temp_dir,
            "config.rs",
            r"
struct Config {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    count: i32,
}

pub fn get_config() -> Config { Config { enabled: true, count: 0 } }
",
        );

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(struct_meta(
            "Config",
            r"struct Config { #[serde(default)] enabled: bool, #[serde(default)] count: i32 }",
        ));
        metadata.routes.push(route_meta(
            "/config",
            "get_config",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
        let config_schema = schemas(&doc).get("Config").expect("Config schema");

        assert_eq!(
            property_default(config_schema, "enabled"),
            Some(&json!(false))
        );
        assert_eq!(property_default(config_schema, "count"), Some(&json!(0)));
    }

    #[test]
    fn struct_file_index_finds_struct_in_another_route_file() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route1_file = create_temp_file(
            &temp_dir,
            "users.rs",
            "pub fn get_users() -> Vec<User> { vec![] }",
        );
        let route2_file = create_temp_file(
            &temp_dir,
            "user.rs",
            r#"
fn default_name() -> String { "Guest".to_string() }

struct User {
    #[serde(default = "default_name")]
    name: String,
}

pub fn get_user() -> User { User { name: "Alice".to_string() } }
"#,
        );

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(struct_meta(
            "User",
            r#"struct User { #[serde(default = "default_name")] name: String }"#,
        ));
        metadata.routes.push(route_meta(
            "/users",
            "get_users",
            &route1_file.to_string_lossy(),
        ));
        metadata.routes.push(route_meta(
            "/user",
            "get_user",
            &route2_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
        let user_schema = schemas(&doc).get("User").expect("User schema");

        assert_eq!(property_default(user_schema, "name"), Some(&json!("Guest")));
    }

    #[test]
    fn stored_field_defaults_have_highest_priority() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let route_file = create_temp_file(
            &temp_dir,
            "config.rs",
            r"
struct Config { count: i32, name: String }
pub fn get_config() -> Config { Config { count: 0, name: String::new() } }
",
        );

        let mut metadata = CollectedMetadata::new();
        metadata.structs.push(StructMetadata {
            name: "Config".to_string(),
            definition: "struct Config { count: i32, name: String }".to_string(),
            include_in_openapi: true,
            explicit_schema_name: false,
            field_defaults: BTreeMap::from([
                ("count".to_string(), json!(42)),
                ("name".to_string(), json!("default_name")),
            ]),
            source_identity: None,
            rust_ident: None,
        });
        metadata.routes.push(route_meta(
            "/config",
            "get_config",
            &route_file.to_string_lossy(),
        ));

        let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
        let config_schema = schemas(&doc).get("Config").expect("Config schema");

        assert_eq!(property_default(config_schema, "count"), Some(&json!(42)));
        assert_eq!(
            property_default(config_schema, "name"),
            Some(&json!("default_name"))
        );
    }
}
