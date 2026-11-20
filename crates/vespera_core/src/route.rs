//! Route-related structure definitions
//!
//! # Reference Objects
//!
//! Only schema positions ([`SchemaRef`]) and Path Item objects
//! ([`PathItem::ref_path`]) accept an OpenAPI Reference Object
//! (`{"$ref": ...}`). [`Parameter`], [`RequestBody`], [`Response`],
//! [`Header`] and [`Example`] are always inline: vespera never emits a
//! reference there, and a document that does (e.g. an external spec with
//! `"200": {"$ref": "#/components/responses/Ok"}`) is rejected when it is
//! deserialized, with or without siblings such as `description`: the `$ref`
//! key is refused rather than silently dropped or read as an empty object.
//! Inline those references before loading such a document into
//! [`crate::openapi::OpenApi`]. Accepting them would need a `RefOr<T>` in
//! every such field, a breaking change of these public types.
//! ([`Link`] is raw JSON, so a referenced link round-trips.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{SchemaRef, openapi::Server, schema::ExternalDocumentation};

/// HTTP method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Trace,
}

impl HttpMethod {
    /// Every supported HTTP method, in `PathItem` field order.
    ///
    /// This array plus [`HttpMethod::as_str`] are the **single source of
    /// truth** for the supported-method set: `Display`, `TryFrom<&str>` and
    /// `vespera_macro`'s `is_http_method` all read the spellings from here, so
    /// adding a variant cannot leave one stage of the pipeline behind.
    pub const ALL: [Self; 8] = [
        Self::Get,
        Self::Post,
        Self::Put,
        Self::Patch,
        Self::Delete,
        Self::Head,
        Self::Options,
        Self::Trace,
    ];

    /// The canonical uppercase wire name of this method (RFC 9110).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Trace => "TRACE",
        }
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deserializes through [`TryFrom<&str>`], so serde accepts the same
/// ASCII-case-insensitive spellings as the rest of the crate (`"get"`,
/// `"Get"`). Serialization stays uppercase.
impl<'de> Deserialize<'de> for HttpMethod {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value.as_str()).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<&str> for HttpMethod {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // Match case-insensitively without allocating an upper-cased copy
        // on the success path (HTTP method names are ASCII per RFC 9110);
        // the cold error path still reports the ASCII-uppercased value so the
        // message is byte-identical to the previous implementation.
        Self::ALL
            .iter()
            .copied()
            .find(|method| value.eq_ignore_ascii_case(method.as_str()))
            .ok_or_else(|| format!("unknown HTTP method: {}", value.to_ascii_uppercase()))
    }
}

/// Parameter location in the request
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterLocation {
    #[default]
    Query,
    Header,
    Path,
    Cookie,
}

/// OpenAPI parameter serialization style (`style` of a Parameter Object).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParameterStyle {
    Matrix,
    Label,
    Form,
    Simple,
    SpaceDelimited,
    PipeDelimited,
    DeepObject,
}

/// Parameter definition (inline only, see [Reference Objects](self#reference-objects))
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", remote = "Self")]
pub struct Parameter {
    /// Parameter name
    pub name: String,
    /// Parameter location
    pub r#in: ParameterLocation,
    /// Parameter description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the parameter is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Schema reference or inline schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
    /// Example value (a JSON `null` example is `Some(Value::Null)`)
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub example: Option<serde_json::Value>,
    /// Serialization style (e.g. `form` for repeated query keys). `None`
    /// means the location's OpenAPI default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ParameterStyle>,
    /// Whether array/object values expand into one parameter per element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explode: Option<bool>,
    /// Whether the parameter is deprecated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    /// Whether an empty value may be sent (query parameters only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_empty_value: Option<bool>,
    /// Whether reserved characters are sent without percent-encoding
    /// (query parameters only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_reserved: Option<bool>,
    /// Named examples (mutually exclusive with `example`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<BTreeMap<String, Example>>,
    /// Media type → schema, used instead of `schema` for complex values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, MediaType>>,
}

/// Request body definition (inline only, see [Reference Objects](self#reference-objects))
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", remote = "Self")]
pub struct RequestBody {
    /// Request body description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the request body is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Schema per Content-Type
    pub content: BTreeMap<String, MediaType>,
}

/// Media type definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaType {
    /// Schema reference or inline schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
    /// Example (a JSON `null` example is `Some(Value::Null)`)
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub example: Option<serde_json::Value>,
    /// Examples
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<BTreeMap<String, Example>>,
    /// Per-property serialization for `multipart` / form request bodies,
    /// keyed by property name.
    ///
    /// `vespera!` leaves this unset: OpenAPI 3.1 already defaults a
    /// `type: string, format: binary` part (the schema of `FieldData` /
    /// `NamedTempFile` fields) to `application/octet-stream`, so emitting it
    /// would only restate the default. Set it when post-processing the
    /// document to narrow a part's media type (e.g. `image/png`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<BTreeMap<String, Encoding>>,
}

/// Encoding of a single `multipart` / form request-body property
/// (OpenAPI 3.1 Encoding Object).
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Encoding {
    /// Content-Type of the part, e.g. `image/png` or `image/png, image/jpeg`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Additional part headers (`Content-Type` is described by
    /// `content_type` and ignored here)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, Header>>,
    /// Serialization style (`application/x-www-form-urlencoded` only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Whether array/object values generate separate parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explode: Option<bool>,
    /// Whether reserved characters are sent without percent-encoding
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_reserved: Option<bool>,
}

/// Example definition (inline only, see [Reference Objects](self#reference-objects))
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", remote = "Self")]
pub struct Example {
    /// Example summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Example description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Example value (a JSON `null` value is `Some(Value::Null)`)
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<serde_json::Value>,
    /// URL of an external example (mutually exclusive with `value`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_value: Option<String>,
}

/// `deserialize_with` for an `example` / `value` field: a present JSON
/// `null` is a real example (`Some(Value::Null)`), told apart from an absent
/// key (`None`, via `#[serde(default)]`) the way [`crate::Schema`] keeps it.
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// `Serialize` / `Deserialize` for an inline-only type (`remote = "Self"`
/// derives the inherent twins used here). A Reference Object is refused by
/// name: it must not be read as the type's own fields, nor reported as a
/// missing required field.
///
/// Known loss (accepted): `Deserialize` buffers the object into a
/// `serde_json::Value` before the typed read, so an error inside the object
/// is re-raised with `D::Error::custom` and reports the enclosing parser's
/// line/column, not the offending key's. Fields of other types keep exact
/// positions. A streaming `$ref` check would avoid it, at the cost of a
/// larger change to this macro.
macro_rules! inline_only_serde {
    ($ty:ident) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $ty::serialize(self, serializer)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                use serde::de::Error as _;
                let value = serde_json::Value::deserialize(deserializer)?;
                if value.get("$ref").is_some() {
                    return Err(D::Error::custom(concat!(
                        "a Reference Object (`$ref`) is not supported as a ",
                        stringify!($ty),
                        " Object; inline the referenced ",
                        stringify!($ty)
                    )));
                }
                $ty::deserialize(value).map_err(D::Error::custom)
            }
        }
    };
}

inline_only_serde!(Example);
inline_only_serde!(Header);
inline_only_serde!(Parameter);
inline_only_serde!(RequestBody);
inline_only_serde!(Response);

/// A Link Object (or a Reference Object to one), kept as raw JSON:
/// vespera never generates links, it only preserves them on round trip.
pub type Link = serde_json::Value;

/// Response definition (inline only, see [Reference Objects](self#reference-objects))
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", remote = "Self")]
pub struct Response {
    /// Response description
    pub description: String,
    /// Header definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, Header>>,
    /// Schema per Content-Type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, MediaType>>,
    /// Operation links reachable from this response, keyed by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<BTreeMap<String, Link>>,
}

/// Header definition (inline only, see [Reference Objects](self#reference-objects))
///
/// `x-*` extension keys are dropped on load (see the `extensions` module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", remote = "Self")]
pub struct Header {
    /// Header description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the header is required
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Whether the header is deprecated
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    /// Schema reference or inline schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
    /// Serialization style (headers only allow `simple`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ParameterStyle>,
    /// Whether array/object values expand
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explode: Option<bool>,
    /// Example value (a JSON `null` example is `Some(Value::Null)`)
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub example: Option<serde_json::Value>,
    /// Named examples (mutually exclusive with `example`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<BTreeMap<String, Example>>,
    /// Media type → schema, used instead of `schema` for complex values
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, MediaType>>,
}

/// `OpenAPI` Callback Object: runtime expression (e.g.
/// `{$request.body#/callbackUrl}`) → the Path Item describing the request
/// the API sends to that URL.
pub type Callback = BTreeMap<String, PathItem>;

/// `OpenAPI` Operation definition
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    /// Operation ID (unique identifier)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// List of tags
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// List of parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    /// Request body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    /// Response definitions (status code -> Response)
    /// (optional in 3.1 — e.g. a webhook or callback operation may omit it;
    /// omitted when empty: a Responses Object must hold at least one
    /// response, so `{}` would be invalid; the Responses Object's `x-*`
    /// extension keys are skipped on deserialize, never read as statuses)
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::extensions::skip_extension_keys"
    )]
    pub responses: BTreeMap<String, Response>,
    /// Security requirements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<crate::schema::SecurityRequirement>>,
    /// Whether this operation is deprecated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    /// External documentation for this operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocumentation>,
    /// Callback definitions: callback name → [`Callback`] (runtime
    /// expression → Path Item), per the OpenAPI 3.1 Callback Object (whose
    /// `x-*` extension keys are skipped on deserialize).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::extensions::skip_callback_extension_keys"
    )]
    pub callbacks: Option<BTreeMap<String, Callback>>,
    /// Alternative servers for this operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<Server>>,
    /// Specification extensions (`x-*` keys only).
    #[serde(flatten, with = "crate::extensions")]
    pub extensions: crate::Extensions,
}

/// Path Item definition (all HTTP methods for a specific path)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathItem {
    /// Reference to another path item.
    #[serde(rename = "$ref", skip_serializing_if = "Option::is_none")]
    pub ref_path: Option<String>,
    /// GET method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    /// POST method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
    /// PUT method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<Operation>,
    /// PATCH method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Operation>,
    /// DELETE method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<Operation>,
    /// HEAD method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<Operation>,
    /// OPTIONS method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Operation>,
    /// TRACE method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Operation>,
    /// Path parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    /// Summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Alternative servers for all operations in this path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<Server>>,
    /// Specification extensions (`x-*` keys only).
    #[serde(flatten, with = "crate::extensions")]
    pub extensions: crate::Extensions,
}

impl PathItem {
    /// Try to set an operation for a specific HTTP method.
    ///
    /// Returns the operation that was already present, if this call replaced one.
    #[must_use]
    pub fn try_set_operation(
        &mut self,
        method: HttpMethod,
        operation: Operation,
    ) -> Option<Operation> {
        match method {
            HttpMethod::Get => self.get.replace(operation),
            HttpMethod::Post => self.post.replace(operation),
            HttpMethod::Put => self.put.replace(operation),
            HttpMethod::Patch => self.patch.replace(operation),
            HttpMethod::Delete => self.delete.replace(operation),
            HttpMethod::Head => self.head.replace(operation),
            HttpMethod::Options => self.options.replace(operation),
            HttpMethod::Trace => self.trace.replace(operation),
        }
    }

    /// Set an operation for a specific HTTP method, discarding any replaced operation.
    pub fn set_operation(&mut self, method: HttpMethod, operation: Operation) {
        let _ = self.try_set_operation(method, operation);
    }

    /// Iterate the operations present on this path item, in a fixed method
    /// order (GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS, TRACE).
    pub fn operations(&self) -> impl Iterator<Item = (HttpMethod, &Operation)> {
        [
            (HttpMethod::Get, self.get.as_ref()),
            (HttpMethod::Post, self.post.as_ref()),
            (HttpMethod::Put, self.put.as_ref()),
            (HttpMethod::Patch, self.patch.as_ref()),
            (HttpMethod::Delete, self.delete.as_ref()),
            (HttpMethod::Head, self.head.as_ref()),
            (HttpMethod::Options, self.options.as_ref()),
            (HttpMethod::Trace, self.trace.as_ref()),
        ]
        .into_iter()
        .filter_map(|(method, op)| op.map(|op| (method, op)))
    }
}

#[cfg(test)]
mod tests;
