//! Serde default functions resolved through a source identity. A sidecar of
//! `component_schemas/tests.rs`, split out to keep that file under the
//! 1000-line cap; `use super::*` keeps its helpers.

use super::*;

/// Two files declare `struct User` with a `#[serde(default = "default_role")]`
/// function each. The struct's default must come from the file its own source
/// identity names, not from whichever same-ident file sorts first.
#[test]
fn serde_default_resolves_in_the_structs_own_file() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let api_file = create_temp_file(
        &temp_dir,
        "api_user.rs",
        r#"
fn default_role() -> String { "guest".to_string() }

struct User {
#[serde(default = "default_role")]
role: String,
}

pub fn get_api_user() -> User { User { role: "x".to_string() } }
"#,
    );
    let models_file = create_temp_file(
        &temp_dir,
        "models_user.rs",
        r#"
fn default_role() -> String { "admin".to_string() }

struct User {
#[serde(default = "default_role")]
role: String,
}

pub fn get_model_user() -> User { User { role: "y".to_string() } }
"#,
    );

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(
        struct_meta(
            "User",
            r#"struct User { #[serde(default = "default_role")] role: String }"#,
        )
        .with_source_identity(format!("{}::User@2:1", models_file.display())),
    );
    metadata.routes.push(route_meta(
        "/api-user",
        "get_api_user",
        &api_file.to_string_lossy(),
    ));
    metadata.routes.push(route_meta(
        "/model-user",
        "get_model_user",
        &models_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let user_schema = schemas(&doc).get("User").expect("User schema");

    assert_eq!(property_default(user_schema, "role"), Some(&json!("admin")));
}

/// The compiler may spell the identity's file differently from the route's
/// normalized key (a `.` component here, `\` on Windows). The struct still
/// resolves its default in its own file.
#[test]
fn serde_default_resolves_through_a_differently_spelled_identity() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let models_file = create_temp_file(
        &temp_dir,
        "models_user.rs",
        r#"
fn default_role() -> String { "admin".to_string() }

struct User {
#[serde(default = "default_role")]
role: String,
}

pub fn get_model_user() -> User { User { role: "y".to_string() } }
"#,
    );
    let spelled = models_file
        .parent()
        .expect("temp file has a parent")
        .join(".")
        .join("models_user.rs");

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(
        struct_meta(
            "User",
            r#"struct User { #[serde(default = "default_role")] role: String }"#,
        )
        .with_source_identity(format!("{}::User@2:1", spelled.display())),
    );
    metadata.routes.push(route_meta(
        "/model-user",
        "get_model_user",
        &models_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let user_schema = schemas(&doc).get("User").expect("User schema");

    assert_eq!(property_default(user_schema, "role"), Some(&json!("admin")));
}
