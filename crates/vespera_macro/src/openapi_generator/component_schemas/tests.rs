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

    let (known_schema_names, struct_definitions) = build_schema_lookups(&metadata, &[]);

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
// An inline non-object payload (`Data(Uuid)`) is named by its wire type.
#[case::inline_string(
    json!({ "allOf": [
        { "type": "object", "properties": { "type": { "type": "string", "enum": ["Data"] } } },
        { "type": "string", "format": "uuid" }
    ] }),
    Some("`string (uuid)` serializes")
)]
#[case::inline_integer(
    json!({ "allOf": [
        { "type": "object", "properties": { "type": { "type": "string", "enum": ["Data"] } } },
        { "type": "integer" }
    ] }),
    Some("`integer` serializes")
)]
#[case::inline_object(
    json!({ "allOf": [
        { "type": "object", "properties": { "type": { "type": "string", "enum": ["Data"] } } },
        { "type": "object", "additionalProperties": { "type": "integer" } }
    ] }),
    None
)]
fn internally_tagged_check_inspects_only_tag_object_plus_local_ref(
    #[case] branch: Value,
    #[case] error_variant: Option<&str>,
) {
    let schemas: BTreeMap<String, vespera_core::schema::Schema> = serde_json::from_value(json!({
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

/// An internally tagged newtype over an inline non-object type (`Uuid`,
/// which `#[derive(Schema)]` cannot tell from a user struct) is a compile
/// error once the schema is built, like a `$ref` to a non-object component;
/// one over a unit-only enum (serde writes `{"type": "A", "On": null}`) is
/// not.
#[rstest]
#[case::uuid(r#"#[serde(tag = "type")] enum Event { Ping, Data(Uuid) }"#, true)]
#[case::unit_only_enum(r#"#[serde(tag = "type")] enum Event { Ping, Data(Mode) }"#, false)]
fn internally_tagged_newtype_over_inline_non_object(#[case] src: &str, #[case] is_error: bool) {
    let mut metadata = CollectedMetadata::new();
    metadata
        .structs
        .push(struct_meta("Mode", "enum Mode { On, Off }"));
    metadata.structs.push(struct_meta("Event", src));
    let result =
        try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    assert_eq!(result.is_err(), is_error, "{src}");
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

    let err = try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[])
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

fn renamed(name: &str, ident: &str, definition: &str) -> StructMetadata {
    StructMetadata {
        rust_ident: Some(ident.to_string()),
        ..StructMetadata::new(name.to_string(), definition.to_string())
    }
}

/// M9: each rename is also registered under its `module::Ident`, so a
/// module-qualified use resolves to the right one; the first scoped entry
/// (by schema name) wins a key two renames share, and a real schema of
/// that key is never replaced.
#[test]
fn scoped_rust_idents_are_registered() {
    let scoped = |modules: &[&str], name: &'static str, definition: &'static str| ScopedRustIdent {
        keys: registry_keys(modules, "Foo"),
        name,
        definition,
    };
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![StructMetadata::new(
        "d::Foo".to_string(),
        "struct Real;".to_string(),
    )];
    let entries = [
        scoped(&["b"], "B", "struct Foo { b: String }"),
        // Two declarations at one exact path (`cfg` twins): the first by name.
        scoped(&["a"], "C", "struct Foo { c: i32 }"),
        scoped(&["a"], "A", "struct Foo { a: i32 }"),
        scoped(&["d"], "D", "struct Foo { d: i32 }"),
    ];
    let (known, definitions) = build_schema_lookups(&metadata, &entries);
    assert!(known.contains("a::Foo") && known.contains("b::Foo"));
    assert!(known.contains("crate::a::Foo") && known.contains("crate::b::Foo"));
    assert_eq!(definitions.get("a::Foo"), Some(&"struct Foo { a: i32 }"));
    assert_eq!(definitions.get("b::Foo"), Some(&"struct Foo { b: String }"));
    assert_eq!(definitions.get("d::Foo"), Some(&"struct Real;"));
}

/// A key two declarations in different modules claim (`models::user::User`
/// and `api::user::User` both reach `user::User`) is registered for neither;
/// the exact `crate::` keys still tell them apart.
#[test]
fn shared_suffix_key_is_registered_for_neither_declaration() {
    let scoped = |modules: &[&str], name: &'static str, definition: &'static str| ScopedRustIdent {
        keys: registry_keys(modules, "User"),
        name,
        definition,
    };
    let entries = [
        scoped(&["models", "user"], "AdminUser", "struct User { id: i32 }"),
        scoped(&["api", "user"], "User", "struct User { name: String }"),
    ];
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![StructMetadata::new(
        "User".to_string(),
        "struct User { name: String }".to_string(),
    )];
    let (known, definitions) = build_schema_lookups(&metadata, &entries);
    assert!(!known.contains("user::User") && !definitions.contains_key("user::User"));
    assert_eq!(
        definitions.get("crate::models::user::User"),
        Some(&"struct User { id: i32 }")
    );
    assert_eq!(
        definitions.get("crate::api::user::User"),
        Some(&"struct User { name: String }")
    );
}

/// Every struct with a source identity gets its module-path keys (renamed or
/// plain), derived from the module around its declaration.
#[test]
fn scoped_rust_idents_come_from_source_identities() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("lib.rs"), "").unwrap();
    let file = dir.path().join("models.rs");
    fs::write(&file, "mod b {\n    struct Foo;\n}\n").unwrap();
    let identity = format!("{}::Foo@2:11", file.display());
    let plain_identity = format!("{}::Plain@4:11", file.display());
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        renamed("B", "Foo", "struct Foo;").with_source_identity(identity),
        renamed("A", "Foo", "struct Foo;"),
        StructMetadata::new("Plain".to_string(), "struct Plain;".to_string())
            .with_source_identity(plain_identity),
    ];
    let scoped = scoped_rust_idents(&metadata);
    let keys: Vec<(&str, &str)> = scoped
        .iter()
        .map(|entry| (entry.keys[0].as_str(), entry.name))
        .collect();
    assert_eq!(
        keys,
        [
            ("crate::models::b::Foo", "B"),
            ("crate::models::Plain", "Plain")
        ]
    );
}

/// Two renamed types sharing a Rust ident resolve the same way whatever
/// order the storage handed them over in.
#[test]
fn shared_rust_ident_alias_is_order_independent() {
    let a = renamed("A", "Foo", "struct Foo { a: i32 }");
    let b = renamed("B", "Foo", "struct Foo { b: String }");
    let resolve = |structs: Vec<StructMetadata>| {
        let mut metadata = CollectedMetadata::new();
        metadata.structs = structs;
        let (_, definitions) = build_schema_lookups(&metadata, &[]);
        definitions.get("Foo").map(|d| (*d).to_string())
    };
    let forward = resolve(vec![a.clone(), b.clone()]);
    assert_eq!(forward, resolve(vec![b, a]));
    assert_eq!(forward.as_deref(), Some("struct Foo { a: i32 }"));
}

#[rstest::rstest]
// A handler signature uses the shared ident → ambiguous.
#[case::used_by_route(
    vec![renamed("A", "Foo", "struct Foo { a: i32 }"), renamed("B", "Foo", "struct Foo { b: i32 }")],
    "async fn h(Json(body): Json<Foo>) -> String",
    Some("ambiguous type `Foo`: `#[schema(name = ...)]` registers it as `A` and `B`")
)]
// Another schema's field uses it → ambiguous.
#[case::used_by_schema(
    vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
        StructMetadata::new("Wrap".to_string(), "struct Wrap { inner: Foo }".to_string()),
    ],
    "async fn h() -> String",
    Some("ambiguous type `Foo`")
)]
// Nothing names `Foo` → harmless.
#[case::unused(
    vec![renamed("A", "Foo", "struct Foo { a: i32 }"), renamed("B", "Foo", "struct Foo { b: i32 }")],
    "async fn h() -> String",
    None
)]
// A real schema named `Foo` wins over both aliases.
#[case::real_schema_wins(
    vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
        StructMetadata::new("Foo".to_string(), "struct Foo { c: i32 }".to_string()),
    ],
    "async fn h(Json(body): Json<Foo>) -> String",
    None
)]
// One rename per ident is never ambiguous.
#[case::single_rename(
    vec![renamed("A", "Foo", "struct Foo { a: i32 }")],
    "async fn h(Json(body): Json<Foo>) -> String",
    None
)]
fn ambiguous_alias_is_rejected_only_when_used(
    #[case] structs: Vec<StructMetadata>,
    #[case] signature: &str,
    #[case] expected: Option<&str>,
) {
    let mut metadata = CollectedMetadata::new();
    metadata.structs = structs;
    let storage = [crate::route_impl::StoredRouteInfo {
        fn_sig_str: signature.to_string(),
        inline_module: None,
        fn_name: "h".to_string(),
        ..Default::default()
    }];
    let result = check_ambiguous_aliases(&metadata, &HashMap::new(), &storage);
    match expected {
        None => assert!(result.is_ok(), "{result:?}"),
        Some(message) => {
            let err = result.expect_err("ambiguous").to_string();
            assert!(err.contains(message), "{err}");
        }
    }
}

/// A route found only through the file cache contributes its signature.
#[test]
fn ambiguous_alias_used_by_a_parsed_route_file() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
    ];
    metadata.routes.push(route_meta("/h", "h", "h.rs"));
    let file_cache = HashMap::from([(
        "h.rs".to_string(),
        syn::parse_file("pub struct X; pub async fn h(Json(b): Json<Foo>) {}").unwrap(),
    )]);
    assert!(check_ambiguous_aliases(&metadata, &file_cache, &[]).is_err());
}

/// A bare `responses = [(404, Foo)]` type is a use of the shared ident too;
/// a module-qualified one picks its type and is not ambiguous.
#[rstest::rstest]
#[case::bare("Foo", true)]
#[case::qualified("crate::b::Foo", false)]
fn ambiguous_alias_used_by_a_typed_response(#[case] path: &str, #[case] ambiguous: bool) {
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
    ];
    let mut route = route_meta("/h", "h", "h.rs");
    route.typed_responses = Some(vec![(404, path.to_string())]);
    metadata.routes.push(route);
    let result = check_ambiguous_aliases(&metadata, &HashMap::new(), &[]);
    assert_eq!(result.is_err(), ambiguous, "{result:?}");
}

/// A module-qualified `responses` type that is no known schema keeps a
/// dangling `$ref` to its last segment: `::` is not valid in a component key.
#[test]
fn unknown_qualified_typed_response_refs_its_last_segment() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let route_file = create_temp_file(
        &temp_dir,
        "h.rs",
        "pub async fn h() -> String { String::new() }",
    );
    let mut metadata = CollectedMetadata::new();
    let mut route = route_meta("/h", "h", &route_file.to_string_lossy());
    route.typed_responses = Some(vec![(404, "crate::errors::NotFound".to_string())]);
    metadata.routes.push(route);
    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let json = serde_json::to_value(&doc.paths["/h"]).unwrap();
    assert_eq!(
        json["get"]["responses"]["404"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/NotFound",
        "{json}"
    );
}

/// Two renamed types sharing a Rust ident each reference THEMSELVES through
/// that ident (`TreeB.children` used to `$ref` the first-sorted `TreeA`).
#[test]
fn renamed_self_reference_resolves_to_its_own_component() {
    let tree = |name: &str| {
        renamed(
            name,
            "Node",
            &format!("#[schema(name = \"{name}\")] struct Node {{ children: Vec<Node> }}"),
        )
    };
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![tree("TreeA"), tree("TreeB")];
    let (known, definitions) = build_schema_lookups(&metadata, &[]);
    let schemas = parse_component_schemas(
        &metadata,
        &known,
        &definitions,
        &HashMap::new(),
        &HashMap::new(),
    )
    .unwrap();
    for name in ["TreeA", "TreeB"] {
        let json = serde_json::to_value(&schemas[name]).unwrap();
        assert_eq!(
            json["properties"]["children"]["items"]["$ref"],
            format!("#/components/schemas/{name}"),
            "{json}"
        );
    }
}

#[test]
fn struct_file_index_is_path_ordered() {
    let file_cache = HashMap::from([
        ("b.rs".to_string(), syn::parse_file("struct Dup;").unwrap()),
        ("a.rs".to_string(), syn::parse_file("struct Dup;").unwrap()),
    ]);
    assert_eq!(
        build_struct_file_index(&file_cache).get("Dup"),
        Some(&"a.rs")
    );
}

#[test]
fn mentions_ident_ignores_unparsable_source() {
    assert!(!mentions_ident("\"unterminated", "Foo"));
    assert!(mentions_ident("fn f(x: Vec<(Foo, u8)>)", "Foo"));
    assert!(mentions_ident("struct S { a: Option<Foo> }", "Foo"));
    // Attributes and module-qualified paths are not ambiguous uses.
    assert!(!mentions_ident(
        "#[derive(vespera::Foo)] struct S { a: super::m::Foo }",
        "Foo"
    ));
    assert!(!mentions_ident("fn f(x: Foobar)", "Foo"));
}

/// Set / `Duration` / `Decimal` fields, enums mixing unit and data
/// variants, `#[form_data(default)]` and one-sided `serde_repr` derives make the request view differ without any `serde`
/// attribute in the whole app.
#[rstest]
#[case::form_data_default("struct Tags { #[form_data(default)] tag: String }", true)]
#[case::one_sided_repr(
    "#[derive(Serialize_repr, Deserialize)] #[repr(u8)] enum Tags { A = 1 }",
    true
)]
#[case::hash_set("struct Tags { tags: std::collections::HashSet<String> }", true)]
#[case::btree_set("struct Tags { tags: BTreeSet<String> }", true)]
#[case::duration("struct Tags { ttl: std::time::Duration }", true)]
#[case::decimal("struct Tags { total: rust_decimal::Decimal }", true)]
#[case::mixed_enum("enum Tags { Empty, Circle { r: f64 } }", true)]
#[case::unit_enum("enum Tags { A, B }", false)]
#[case::plain("struct Tags { tags: Vec<String> }", false)]
fn input_views_without_serde_attributes(#[case] definition: &str, #[case] differs: bool) {
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![struct_meta("Tags", definition)];
    let names = input_names(&metadata, &HashSet::from(["Tags"]), &[]);
    assert_eq!(names.contains_key("Tags"), differs, "{names:?}");
}

/// Materialised request-view generic instances (`Tree_i32.Input`) no request
/// body reaches are pruned with the `{Name}Input` components; reached ones
/// stay.
#[test]
fn unreached_materialised_input_instances_are_pruned() {
    let schema =
        |value: Value| serde_json::from_value::<vespera_core::schema::Schema>(value).unwrap();
    let tree_ref =
        json!({"type": "array", "items": {"$ref": "#/components/schemas/Tree_i32.Input"}});
    let mut schemas = BTreeMap::from([
        ("Forest".to_string(), schema(json!({"type": "object"}))),
        (
            "ForestInput".to_string(),
            schema(json!({"type": "object", "properties": {"trees": tree_ref}})),
        ),
        (
            "Tree_i32.Input".to_string(),
            schema(json!({"type": "object"})),
        ),
        (
            "Leaf_u8.Input".to_string(),
            schema(json!({"type": "object"})),
        ),
    ]);
    let names: request_view::InputNames = std::sync::Arc::new(
        BTreeMap::from([("Forest".to_string(), "ForestInput".to_string())]).into(),
    );
    let mut response_only = schemas.clone();
    prune_unreferenced_inputs(&mut response_only, &BTreeMap::new(), &names);
    assert_eq!(response_only.keys().collect::<Vec<_>>(), ["Forest"]);

    let paths: BTreeMap<String, vespera_core::route::PathItem> = BTreeMap::from([(
        "/forests".to_string(),
        serde_json::from_value(
            json!({"post": {"requestBody": {"content": {"application/json": {
            "schema": {"$ref": "#/components/schemas/ForestInput"}
        }}}, "responses": {}}}),
        )
        .unwrap(),
    )]);
    prune_unreferenced_inputs(&mut schemas, &paths, &names);
    assert_eq!(
        schemas.keys().collect::<Vec<_>>(),
        ["Forest", "ForestInput", "Tree_i32.Input"]
    );
}

/// A response property named `parameters` / `requestBody` is not an
/// operation key: the type it references is not request-reachable, so its
/// `.Input` twin is pruned. The same key under an operation does reach it.
#[test]
fn response_property_named_like_a_request_key_does_not_reach_input() {
    let names: request_view::InputNames = std::sync::Arc::new(
        BTreeMap::from([("Filter".to_string(), "FilterInput".to_string())]).into(),
    );
    let schema =
        |value: Value| serde_json::from_value::<vespera_core::schema::Schema>(value).unwrap();
    let base = || {
        BTreeMap::from([
            ("Filter".to_string(), schema(json!({"type": "object"}))),
            ("FilterInput".to_string(), schema(json!({"type": "object"}))),
        ])
    };
    let path = |value: serde_json::Value| {
        BTreeMap::from([(
            "/search".to_string(),
            serde_json::from_value::<vespera_core::route::PathItem>(value).unwrap(),
        )])
    };
    let response_body = json!({"type": "object", "properties": {
        "parameters": {"$ref": "#/components/schemas/Filter"},
        "requestBody": {"$ref": "#/components/schemas/Filter"}
    }});
    let mut schemas = base();
    let paths = path(json!({"get": {"responses": {"200": {"description": "ok",
        "content": {"application/json": {"schema": response_body}}}}}}));
    prune_unreferenced_inputs(&mut schemas, &paths, &names);
    assert_eq!(schemas.keys().collect::<Vec<_>>(), ["Filter"]);

    let mut schemas = base();
    let query = json!({"name": "q", "in": "query",
        "schema": {"$ref": "#/components/schemas/FilterInput"}});
    let paths = path(json!({"get": {"parameters": [query.clone()], "responses": {}}}));
    prune_unreferenced_inputs(&mut schemas, &paths, &names);
    assert_eq!(
        schemas.keys().collect::<Vec<_>>(),
        ["Filter", "FilterInput"]
    );

    // Path-level parameters apply to every operation of the path item.
    let mut schemas = base();
    let paths = path(json!({"parameters": [query]}));
    prune_unreferenced_inputs(&mut schemas, &paths, &names);
    assert_eq!(
        schemas.keys().collect::<Vec<_>>(),
        ["Filter", "FilterInput"]
    );
}

/// Two modules sharing the leaf name `user`: `src/models/user.rs` renames its
/// `User` to `AdminUser`, `src/api/user.rs` keeps a plain `User`. A qualified
/// reference names the declaration in its own module; a relative `user::User`
/// is ambiguous between the two and falls back to the bare ident.
#[rstest]
#[case::api_anchored("crate::api::user::User", "#/components/schemas/User")]
#[case::models_anchored("crate::models::user::User", "#/components/schemas/AdminUser")]
#[case::relative_ambiguous("user::User", "#/components/schemas/User")]
#[case::bare_ident("User", "#/components/schemas/User")]
fn same_leaf_modules_resolve_to_their_own_declaration(#[case] ty: &str, #[case] expected: &str) {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(src.join("models")).unwrap();
    fs::create_dir_all(src.join("api")).unwrap();
    fs::write(src.join("lib.rs"), "").unwrap();
    let models = src.join("models").join("user.rs");
    let api = src.join("api").join("user.rs");
    fs::write(&models, "pub struct User { id: i32 }").unwrap();
    fs::write(&api, "pub struct User { name: String }").unwrap();
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        renamed(
            "AdminUser",
            "User",
            "#[schema(name = \"AdminUser\")] struct User { id: i32 }",
        )
        .with_source_identity(format!("{}::User@1:11", models.display())),
        StructMetadata::new(
            "User".to_string(),
            "struct User { name: String }".to_string(),
        )
        .with_source_identity(format!("{}::User@1:11", api.display())),
    ];
    let scoped = scoped_rust_idents(&metadata);
    let (known, definitions) = build_schema_lookups(&metadata, &scoped);
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    let schema = crate::parser::schema::parse_type_to_schema_ref(&ty, &known, &definitions);
    let SchemaRef::Ref(reference) = schema else {
        panic!("expected a $ref, got {schema:?}");
    };
    assert_eq!(reference.ref_path, expected);
}

/// With the plain `User` absent, the leaf is unambiguous: a relative
/// `user::User` resolves to the renamed declaration of its module.
#[test]
fn unambiguous_relative_module_path_resolves_to_the_rename() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(src.join("models")).unwrap();
    fs::write(src.join("lib.rs"), "").unwrap();
    let models = src.join("models").join("user.rs");
    fs::write(&models, "pub struct User { id: i32 }").unwrap();
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        renamed(
            "AdminUser",
            "User",
            "#[schema(name = \"AdminUser\")] struct User { id: i32 }",
        )
        .with_source_identity(format!("{}::User@1:11", models.display())),
    ];
    let scoped = scoped_rust_idents(&metadata);
    let (known, definitions) = build_schema_lookups(&metadata, &scoped);
    let ty: syn::Type = syn::parse_str("user::User").unwrap();
    let schema = crate::parser::schema::parse_type_to_schema_ref(&ty, &known, &definitions);
    let SchemaRef::Ref(reference) = schema else {
        panic!("expected a $ref, got {schema:?}");
    };
    assert_eq!(reference.ref_path, "#/components/schemas/AdminUser");
}

#[path = "tests_identity.rs"]
mod tests_identity;
