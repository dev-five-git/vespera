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

fn path_item_refs(paths: serde_json::Value, path_items: serde_json::Value) -> serde_json::Value {
    let mut out = json!({"components": {}});
    out["paths"] = paths;
    out["components"]["pathItems"] = path_items;
    out
}

#[rstest::rstest]
#[case::child_security_reaches_the_referenced_operation(
    json!({"security": [{"bearerAuth": []}]}),
    Some(bearer())
)]
#[case::unauthenticated_child_operation_stays_public(json!({}), Some(Vec::new()))]
fn ref_path_item_is_inlined_for_the_security_push_down(
    #[case] child_top_level: serde_json::Value,
    #[case] expected: Option<Vec<SecurityRequirement>>,
) {
    let mut parent = doc(json!({"security": [{"apiKey": []}]}));
    let mut child = path_item_refs(
        json!({"/a": {"$ref": "#/components/pathItems/A"}}),
        json!({"A": {"summary": "a", "get": {"responses": {}}}}),
    );
    child
        .as_object_mut()
        .unwrap()
        .extend(child_top_level.as_object().unwrap().clone());
    parent.merge(doc(child));
    let item = &parent.paths["/a"];
    assert_eq!(item.ref_path, None);
    assert_eq!(item.summary.as_deref(), Some("a"));
    assert_eq!(op(&parent, "/a", "get").security, expected);
    // The shared component itself is copied over unchanged.
    let component = &parent
        .components
        .as_ref()
        .unwrap()
        .path_items
        .as_ref()
        .unwrap()["A"];
    assert_eq!(component.get.as_ref().unwrap().security, None);
}

#[test]
fn ref_path_item_and_webhook_get_the_servers_and_follow_a_reference_chain() {
    let mut parent = doc(json!({}));
    let mut child = path_item_refs(
        json!({"/a": {"$ref": "#/components/pathItems/A"}}),
        json!({
            "A": {"$ref": "#/components/pathItems/B"},
            "B": {"get": {"responses": {}}},
            "H": {"post": {"responses": {}}}
        }),
    );
    let fields = child.as_object_mut().unwrap();
    fields.insert(
        "servers".to_owned(),
        json!([{"url": "https://child.example"}]),
    );
    fields.insert("security".to_owned(), json!([{"bearerAuth": []}]));
    fields.insert(
        "webhooks".to_owned(),
        json!({"hook": {"$ref": "#/components/pathItems/H"}}),
    );
    parent.merge(doc(child));
    let get = op(&parent, "/a", "get");
    assert_eq!(
        get.servers.as_ref().unwrap()[0].url,
        "https://child.example"
    );
    assert_eq!(get.security, Some(bearer()));
    let hook = &parent.webhooks.as_ref().unwrap()["hook"];
    assert_eq!(hook.ref_path, None);
    let post = hook.post.as_ref().unwrap();
    assert_eq!(post.security, Some(bearer()));
    assert!(post.servers.is_none());
}

#[rstest::rstest]
#[case::missing_component("#/components/pathItems/Missing")]
#[case::external_reference("other.yaml#/paths/~1a")]
#[case::reference_cycle("#/components/pathItems/A")]
fn unresolvable_ref_path_item_is_left_as_a_reference(#[case] target: &str) {
    let mut parent = doc(json!({"security": [{"apiKey": []}]}));
    parent.merge(doc(path_item_refs(
        json!({"/a": {"$ref": target}}),
        json!({"A": {"$ref": "#/components/pathItems/B"}, "B": {"$ref": "#/components/pathItems/A"}}),
    )));
    assert_eq!(parent.paths["/a"].ref_path.as_deref(), Some(target));
}

#[test]
fn ref_path_item_stays_a_reference_when_nothing_is_pushed_down() {
    let mut parent = doc(json!({}));
    parent.merge(doc(path_item_refs(
        json!({"/a": {"$ref": "#/components/pathItems/A"}}),
        json!({"A": {"get": {"responses": {}}}}),
    )));
    let item = &parent.paths["/a"];
    assert_eq!(item.ref_path.as_deref(), Some("#/components/pathItems/A"));
    assert!(item.get.is_none());
}

#[test]
fn ref_path_items_without_components_are_left_alone() {
    let mut parent = doc(json!({"security": [{"apiKey": []}]}));
    parent.merge(doc(
        json!({"paths": {"/a": {"$ref": "#/components/pathItems/A"}}}),
    ));
    assert_eq!(
        parent.paths["/a"].ref_path.as_deref(),
        Some("#/components/pathItems/A")
    );
}

/// Round 11: on a colliding path, a `#/components/pathItems/*` reference of
/// either side is inlined even when nothing is pushed down, so the merged
/// item never carries a `$ref` next to operations.
#[test]
fn colliding_ref_path_items_are_inlined_on_both_sides() {
    let mut parent =
        doc(json!({"paths": {"/users": {"get": {"summary": "parent", "responses": {}}}}}));
    parent.merge(doc(path_item_refs(
        json!({"/users": {"$ref": "#/components/pathItems/U"}}),
        json!({"U": {"get": {"summary": "child", "responses": {}}, "post": {"responses": {}}}}),
    )));
    let item = &parent.paths["/users"];
    assert_eq!(item.ref_path, None);
    assert_eq!(
        op(&parent, "/users", "get").summary.as_deref(),
        Some("parent")
    );
    assert!(item.post.is_some());

    let mut parent = doc(path_item_refs(
        json!({"/users": {"$ref": "#/components/pathItems/U"}}),
        json!({"U": {"get": {"summary": "parent", "responses": {}}}}),
    ));
    parent.merge(doc(json!({"paths": {"/users": {"get": {"summary": "child", "responses": {}}, "post": {"responses": {}}}}})));
    let item = &parent.paths["/users"];
    assert_eq!(item.ref_path, None);
    assert_eq!(
        op(&parent, "/users", "get").summary.as_deref(),
        Some("parent")
    );
    assert!(item.post.is_some());
}

/// An unresolvable reference is never combined with operations: the
/// receiver keeps its operations over an incoming reference, and a receiver
/// that is still a reference keeps itself over incoming operations.
#[test]
fn unresolvable_ref_is_never_merged_next_to_operations() {
    let mut parent = doc(json!({"paths": {"/a": {"get": {"responses": {}}}}}));
    parent.merge(doc(
        json!({"paths": {"/a": {"$ref": "other.yaml#/a", "summary": "s"}}}),
    ));
    let item = &parent.paths["/a"];
    assert_eq!(item.ref_path, None);
    assert!(item.get.is_some());
    assert_eq!(item.summary.as_deref(), Some("s"));

    let mut parent = doc(json!({"paths": {"/a": {"$ref": "other.yaml#/a"}}}));
    parent.merge(doc(json!({"paths": {"/a": {"post": {"responses": {}}}}})));
    let item = &parent.paths["/a"];
    assert_eq!(item.ref_path.as_deref(), Some("other.yaml#/a"));
    assert!(item.post.is_none());
}
