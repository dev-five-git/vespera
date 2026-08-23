use serde::{Deserialize, Serialize};
use vespera_macro::Schema;

fn compute_tags() -> Vec<String> {
    vec!["runtime".to_string()]
}

fn non_literal_default() -> Vec<String> {
    compute_tags()
}

#[derive(Serialize, Deserialize, Schema)]
pub struct Request {
    #[serde(default = "non_literal_default")]
    pub tags: Vec<String>,
}

fn main() {}
