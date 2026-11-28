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
//!
//! **Known limit:** the objects below do not carry the map, so their `x-*`
//! keys are dropped on deserialize (not stored, not re-emitted): `Components`,
//! `Parameter`, `RequestBody`, `Response`, `Header`, `Example`, `Encoding`,
//! `SecurityScheme`, `OAuthFlows`, `OAuthFlow`, `Contact`, `License`,
//! `ServerVariable`, `Server`, `Tag`, `MediaType`, `Schema` and
//! `ExternalDocumentation`. Adding the field to them would
//! add a public field to literal-constructible structs (a breaking change for
//! a Patch release), so the limit is kept until the next breaking release.

use crate::route::{Callback, PathItem};
use serde::de::{DeserializeOwned, Error};
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

/// `deserialize_with` for a Paths / Responses / Callback map: the `x-*`
/// keys are dropped before the rest is read as `V` (an extension value is
/// never parsed as a Path Item / Response).
pub fn skip_extension_keys<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: DeserializeOwned,
{
    without_extensions(Extensions::deserialize(deserializer)?)
}

fn without_extensions<V: DeserializeOwned, E: Error>(
    raw: Extensions,
) -> Result<BTreeMap<String, V>, E> {
    raw.into_iter()
        .filter(|(key, _)| !is_extension(key))
        .map(|(key, value)| match V::deserialize(value) {
            Ok(value) => Ok((key, value)),
            // The map was buffered, so the error carries no position: name
            // the entry (nested maps prefix theirs, e.g. `/users: 200: ...`).
            Err(err) => Err(E::custom(format_args!("{key}: {err}"))),
        })
        .collect()
}

/// `deserialize_with` for an optional `callbacks` map (callback name ->
/// Callback Object): every Callback's `x-*` keys are skipped.
pub fn skip_callback_extension_keys<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, Callback>>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(callbacks) = Option::<BTreeMap<String, Extensions>>::deserialize(deserializer)? else {
        return Ok(None);
    };
    callbacks
        .into_iter()
        .map(|(name, callback)| {
            // A Callback takes no Reference Object (route docs): a `$ref`
            // key is refused here, bare or with siblings, instead of being
            // read as a runtime expression (`{"$ref": {}}` would load as an
            // empty Path Item).
            if callback.contains_key("$ref") {
                return Err(D::Error::custom(format_args!(
                    "{name}: a Reference Object (`$ref`) is not supported as a Callback Object; inline the referenced Callback"
                )));
            }
            // Name the callback as well as the runtime expression that
            // without_extensions prefixes: `onEvent: {$url}: ...`.
            without_extensions::<PathItem, D::Error>(callback)
                .map_err(|err| D::Error::custom(format_args!("{name}: {err}")))
                .map(|callback| (name, callback))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}
