//! OpenAPI Specification Extensions (`x-*` fields).
//!
//! Objects that support them carry a
//! `#[serde(flatten, with = "crate::extensions")] extensions` map: on
//! deserialize it keeps only the keys starting with `x-` (every other
//! unknown key is ignored as before); on serialize it writes only those
//! keys, so a hand-inserted non-`x-` key can never shadow a real field.
//!
//! The Paths, Responses and Callback objects are plain maps here (path /
//! status code / runtime expression → value), so their `x-*` keys are
//! skipped on deserialize instead ([`skip_extension_keys`]): an extension
//! value is never mistaken for a Path Item / Response, and never comes back
//! as a bogus route or status.

use crate::route::{Callback, PathItem};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

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

/// A map of an extensible OpenAPI object whose `x-*` keys are skipped (their
/// values are not parsed as `V`); every other key is deserialized as `V`.
pub struct WithoutExtensions<V>(pub BTreeMap<String, V>);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for WithoutExtensions<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<V>(PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
            type Value = WithoutExtensions<V>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut map = BTreeMap::new();
                while let Some(key) = access.next_key::<String>()? {
                    if is_extension(&key) {
                        access.next_value::<IgnoredAny>()?;
                    } else {
                        map.insert(key, access.next_value()?);
                    }
                }
                Ok(WithoutExtensions(map))
            }
        }

        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

/// `deserialize_with` for a Paths / Responses / Callback map: see
/// [`WithoutExtensions`].
pub fn skip_extension_keys<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    WithoutExtensions::deserialize(deserializer).map(|map| map.0)
}

/// `deserialize_with` for an optional `callbacks` map (callback name →
/// Callback Object): every Callback's `x-*` keys are skipped.
pub fn skip_callback_extension_keys<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, Callback>>, D::Error>
where
    D: Deserializer<'de>,
{
    let callbacks =
        Option::<BTreeMap<String, WithoutExtensions<PathItem>>>::deserialize(deserializer)?;
    Ok(callbacks.map(|callbacks| {
        callbacks
            .into_iter()
            .map(|(name, callback)| (name, callback.0))
            .collect()
    }))
}
