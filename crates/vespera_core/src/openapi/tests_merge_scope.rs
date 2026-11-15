//! `OpenApi::merge` must not change which requests a merged operation
//! describes: the receiver's top-level `security` and either side's
//! path-level `parameters` / `servers` stay with the operations they governed.

use super::*;
use crate::route::{Operation, Parameter, ParameterLocation};
use crate::schema::SecurityRequirement;
use serde_json::json;

fn doc(value: serde_json::Value) -> OpenApi {
    let mut base = json!({"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {}});
    if let serde_json::Value::Object(fields) = value {
        base.as_object_mut().unwrap().extend(fields);
    }
    serde_json::from_value(base).unwrap()
}

fn op<'a>(api: &'a OpenApi, path: &str, method: &str) -> &'a Operation {
    let item = api.paths.get(path).expect("path present");
    item.operations()
        .find(|(m, _)| m.as_str().eq_ignore_ascii_case(method))
        .map(|(_, operation)| operation)
        .expect("operation present")
}

fn bearer() -> Vec<SecurityRequirement> {
    vec![SecurityRequirement::from([(
        "bearerAuth".to_owned(),
        Vec::new(),
    )])]
}

fn param_names(operation: &Operation) -> Vec<(String, ParameterLocation)> {
    operation
        .parameters
        .iter()
        .flatten()
        .map(|p: &Parameter| (p.name.clone(), p.r#in))
        .collect()
}

#[test]
fn receiver_security_does_not_start_governing_an_unauthenticated_child_operation() {
    let mut parent = doc(json!({"security": [{"bearerAuth": []}]}));
    let child = doc(json!({"paths": {
        "/health": {"get": {"responses": {}}},
        "/me": {"get": {"responses": {}, "security": [{"apiKey": []}]}}
    }}));
    parent.merge(child);
    // Unauthenticated in the child: explicitly so after the merge.
    assert_eq!(op(&parent, "/health", "get").security, Some(Vec::new()));
    // An operation with its own requirement keeps it.
    assert_eq!(
        op(&parent, "/me", "get").security,
        Some(vec![SecurityRequirement::from([(
            "apiKey".to_owned(),
            Vec::new()
        )])])
    );
    // The receiver's own top level is untouched.
    assert_eq!(parent.security, Some(bearer()));
}

#[test]
fn child_security_still_wins_over_the_receiver_requirement() {
    let mut parent = doc(json!({"security": [{"apiKey": []}]}));
    let child = doc(json!({
        "security": [{"bearerAuth": []}],
        "paths": {"/x": {"get": {"responses": {}}}}
    }));
    parent.merge(child);
    assert_eq!(op(&parent, "/x", "get").security, Some(bearer()));
}

#[rstest::rstest]
#[case::receiver_without_security(json!({}))]
#[case::receiver_with_empty_security(json!({"security": []}))]
fn child_operation_stays_unset_when_the_receiver_requires_nothing(
    #[case] receiver: serde_json::Value,
) {
    let mut parent = doc(receiver);
    parent.merge(doc(json!({"paths": {"/x": {"get": {"responses": {}}}}})));
    assert_eq!(op(&parent, "/x", "get").security, None);
}

#[test]
fn webhooks_get_the_security_push_down_but_never_the_api_servers() {
    let mut parent = doc(json!({"security": [{"bearerAuth": []}]}));
    let child = doc(json!({
        "servers": [{"url": "https://child.example"}],
        "paths": {"/x": {"get": {"responses": {}}}},
        "webhooks": {"newPet": {"post": {"responses": {}}}}
    }));
    parent.merge(child);
    let hook = parent.webhooks.as_ref().unwrap()["newPet"]
        .post
        .as_ref()
        .unwrap();
    assert_eq!(hook.servers.as_ref().map(Vec::len), None);
    assert_eq!(hook.security, Some(Vec::new()));
    // Regular paths still get the child's top-level servers.
    let servers = op(&parent, "/x", "get").servers.as_ref().unwrap();
    assert_eq!(servers[0].url, "https://child.example");
}

#[test]
fn colliding_path_keeps_each_sides_path_level_parameters_and_servers_on_its_own_operations() {
    let mut parent = doc(json!({"paths": {"/users": {
        "parameters": [{"name": "X-Org", "in": "header"}],
        "servers": [{"url": "https://parent.example"}],
        "get": {"responses": {}}
    }}}));
    let child = doc(json!({"paths": {"/users": {
        "parameters": [
            {"name": "X-Tenant", "in": "header"},
            {"name": "id", "in": "query", "description": "path-level"}
        ],
        "servers": [{"url": "https://child.example"}],
        "post": {
            "responses": {},
            "parameters": [{"name": "id", "in": "query", "description": "op-level"}]
        }
    }}}));
    parent.merge(child);

    let item = &parent.paths["/users"];
    assert!(
        item.parameters.is_none(),
        "no path-level parameter may cross over"
    );
    assert!(
        item.servers.is_none(),
        "no path-level server may cross over"
    );

    let get = op(&parent, "/users", "get");
    assert_eq!(
        param_names(get),
        [("X-Org".to_owned(), ParameterLocation::Header)]
    );
    assert_eq!(
        get.servers.as_ref().unwrap()[0].url,
        "https://parent.example"
    );

    let post = op(&parent, "/users", "post");
    assert_eq!(
        param_names(post),
        [
            ("id".to_owned(), ParameterLocation::Query),
            ("X-Tenant".to_owned(), ParameterLocation::Header),
        ]
    );
    // The operation-level `id` overrides the path-level one.
    assert_eq!(
        post.parameters.as_ref().unwrap()[0].description.as_deref(),
        Some("op-level")
    );
    assert_eq!(
        post.servers.as_ref().unwrap()[0].url,
        "https://child.example"
    );
}

#[test]
fn colliding_path_whose_incoming_operations_are_all_shadowed_drops_their_path_level_fields() {
    let mut parent = doc(json!({"paths": {"/users": {"get": {"responses": {}}}}}));
    let child = doc(json!({"paths": {"/users": {
        "parameters": [{"name": "X-Tenant", "in": "header"}],
        "servers": [{"url": "https://child.example"}],
        "get": {"responses": {}}
    }}}));
    parent.merge(child);
    let item = &parent.paths["/users"];
    assert!(item.parameters.is_none());
    assert!(item.servers.is_none());
    let get = op(&parent, "/users", "get");
    assert!(get.parameters.is_none());
    assert!(get.servers.is_none());
}

#[test]
fn colliding_path_without_receiver_operations_adopts_the_path_level_fields_as_is() {
    let mut parent = doc(json!({"paths": {"/users": {"summary": "users"}}}));
    let child = doc(json!({"paths": {"/users": {
        "parameters": [{"name": "X-Tenant", "in": "header"}],
        "post": {"responses": {}}
    }}}));
    parent.merge(child);
    let item = &parent.paths["/users"];
    assert_eq!(item.parameters.as_ref().map(Vec::len), Some(1));
    assert!(op(&parent, "/users", "post").parameters.is_none());
}

#[test]
fn colliding_path_push_down_keeps_operations_without_path_level_fields_unchanged() {
    let mut parent = doc(json!({"paths": {"/users": {"get": {"responses": {}}}}}));
    let child = doc(json!({"paths": {"/users": {
        "post": {"responses": {}, "servers": [{"url": "https://op.example"}]}
    }}}));
    parent.merge(child);
    assert!(op(&parent, "/users", "get").parameters.is_none());
    assert!(op(&parent, "/users", "get").servers.is_none());
    let post = op(&parent, "/users", "post");
    assert!(post.parameters.is_none());
    assert_eq!(post.servers.as_ref().unwrap()[0].url, "https://op.example");
}

/// An operation-less receiver's path-level values govern nothing: they must
/// not start governing the incoming operations, and the incoming item's own
/// path-level values must not be lost behind them.
#[rstest::rstest]
#[case::parameters(json!({"parameters": [{"name": "X-Org", "in": "header"}]}))]
#[case::servers(json!({"servers": [{"url": "https://parent.example"}]}))]
fn colliding_path_with_receiver_path_level_but_no_operations_keeps_the_incoming_scope(
    #[case] receiver_item: serde_json::Value,
) {
    let mut parent = doc(json!({"paths": {"/u": receiver_item}}));
    let child = doc(json!({"paths": {"/u": {
        "parameters": [{"name": "X-Tenant", "in": "header"}],
        "servers": [{"url": "https://child.example"}],
        "post": {"responses": {}}
    }}}));
    parent.merge(child);
    let item = &parent.paths["/u"];
    assert!(item.parameters.is_none());
    assert!(item.servers.is_none());
    let post = op(&parent, "/u", "post");
    assert_eq!(
        param_names(post),
        vec![("X-Tenant".to_owned(), ParameterLocation::Header)]
    );
    assert_eq!(
        post.servers.as_ref().unwrap()[0].url,
        "https://child.example"
    );
}
