//! Std leaves a `#[derive(Schema)]` field can hold without a `Schema` impl of
//! their own: `BinaryHeap<T>`, a sequence like `VecDeque` (M44-MR4-1), and the
//! bare `Map` of `use serde_json::Map;`, a JSON object (L44-MR4-2). The fields
//! used to fail the compile-time `T: Schema` assertion (`BinaryHeap`) or
//! documented a dangling `$ref` (`Map`).
#![deny(warnings)]

use vespera::Schema;
use vespera::serde_json::{Map, Value, to_value};

#[derive(Schema)]
pub struct Job {
    pub id: u32,
}

#[derive(Schema)]
pub struct Queue {
    pub priorities: std::collections::BinaryHeap<u32>,
    pub jobs: std::collections::BinaryHeap<Job>,
    pub queue: std::collections::VecDeque<u32>,
    pub pending: std::collections::VecDeque<Job>,
    pub extra: Map<String, u8>,
    pub any: Map<String, Value>,
    pub by_id: Map<String, Job>,
}

fn property(name: &str) -> Value {
    let schema = vespera::schema!(Queue);
    let properties = schema.properties.as_ref().expect("properties");
    to_value(&properties[name]).expect("schema serializes")
}

#[test]
fn binary_heap_of_a_builtin_documents_an_array_of_its_element() {
    let priorities = property("priorities");
    assert_eq!(priorities["type"], "array");
    assert_eq!(priorities["items"]["type"], "integer");
}

#[test]
fn binary_heap_of_a_struct_documents_an_array_of_its_reference() {
    let jobs = property("jobs");
    assert_eq!(jobs["type"], "array");
    assert!(
        jobs["items"]["$ref"]
            .as_str()
            .is_some_and(|reference| reference.ends_with("Job")),
        "{jobs}"
    );
}

#[test]
fn vecdeque_documents_like_binary_heap() {
    let queue = property("queue");
    assert_eq!(queue["type"], "array");
    assert_eq!(queue["items"]["type"], "integer");
    let pending = property("pending");
    assert_eq!(pending["type"], "array");
    assert!(
        pending["items"]["$ref"]
            .as_str()
            .is_some_and(|reference| reference.ends_with("Job")),
        "{pending}"
    );
}

#[test]
fn bare_serde_json_map_documents_a_json_object() {
    let any = property("any");
    assert_eq!(any["type"], "object", "{any}");
    assert_eq!(any["additionalProperties"], true, "{any}");
    // A typed value is the member schema: `u8` is a `uint8` integer.
    let extra = property("extra");
    assert_eq!(extra["type"], "object", "{extra}");
    assert_eq!(extra["additionalProperties"]["type"], "integer", "{extra}");
    assert_eq!(extra["additionalProperties"]["format"], "uint8", "{extra}");
}

#[test]
fn serde_json_map_of_a_struct_documents_a_ref_member() {
    let by_id = property("by_id");
    assert_eq!(by_id["type"], "object", "{by_id}");
    assert!(
        by_id["additionalProperties"]["$ref"]
            .as_str()
            .is_some_and(|reference| reference.ends_with("Job")),
        "{by_id}"
    );
}
