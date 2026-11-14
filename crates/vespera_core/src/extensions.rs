//! OpenAPI Specification Extensions (`x-*` fields).
//!
//! Objects that support them carry a
//! `#[serde(flatten, with = "crate::extensions")] extensions` map: on
//! deserialize it keeps only the keys starting with `x-` (every other
//! unknown key is ignored as before); on serialize it writes only those
//! keys, so a hand-inserted non-`x-` key can never shadow a real field.

use serde::{Deserialize, Deserializer, Serializer};
use std::collections::BTreeMap;

/// Specification extensions of one object: `x-*` key → any JSON value.
pub type Extensions = BTreeMap<String, serde_json::Value>;

fn is_extension(key: &str) -> bool {
    key.starts_with("x-")
}

pub fn serialize<S>(extensions: &Extensions, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.collect_map(extensions.iter().filter(|(key, _)| is_extension(key)))
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Extensions, D::Error>
where
    D: Deserializer<'de>,
{
    let mut extensions = Extensions::deserialize(deserializer)?;
    extensions.retain(|key, _| is_extension(key));
    Ok(extensions)
}
