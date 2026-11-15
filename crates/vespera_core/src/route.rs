//! Route-related structure definitions
//!
//! # Reference Objects
//!
//! Only schema positions ([`SchemaRef`]) accept an OpenAPI Reference Object
//! (`{"$ref": ...}`). [`Parameter`], [`RequestBody`], [`Response`],
//! [`Header`] and [`Example`] are always inline: vespera never emits a
//! reference there, and a document that does (e.g. an external spec with
//! `"200": {"$ref": "#/components/responses/Ok"}`) fails to deserialize
//! (`missing field ...` for the types with a required field; [`Header`] and
//! [`Example`], whose fields are all optional, reject the `$ref` key itself
//! instead of reading the reference as an empty object). Inline those
//! references before loading such a
//! document into [`crate::openapi::OpenApi`]. Accepting them would need a
//! `RefOr<T>` in every such field, a breaking change of these public types.
//! ([`Link`] is raw JSON, so a referenced link round-trips.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{SchemaRef, openapi::Server, schema::ExternalDocumentation};

/// HTTP method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// `Serialize` / `Deserialize` for an inline-only type whose fields are all
/// optional (`remote = "Self"` derives the inherent twins used here): a
/// Reference Object would otherwise deserialize as an empty object, losing
/// the reference silently, so its `$ref` key is rejected.
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

/// A Link Object (or a Reference Object to one), kept as raw JSON:
/// vespera never generates links, it only preserves them on round trip.
pub type Link = serde_json::Value;

/// Response definition (inline only, see [Reference Objects](self#reference-objects))
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn media_type_encoding_serializes_only_when_set() {
        let mut media = MediaType {
            schema: None,
            example: None,
            examples: None,
            encoding: None,
        };
        assert_eq!(serde_json::to_string(&media).unwrap(), "{}");

        media.encoding = Some(BTreeMap::from([(
            "avatar".to_string(),
            Encoding {
                content_type: Some("image/png".to_string()),
                allow_reserved: Some(false),
                ..Encoding::default()
            },
        )]));
        let json = serde_json::to_string(&media).unwrap();
        assert_eq!(
            json,
            r#"{"encoding":{"avatar":{"contentType":"image/png","allowReserved":false}}}"#
        );
        let parsed: MediaType = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.encoding.unwrap()["avatar"].content_type.as_deref(),
            Some("image/png")
        );
    }

    fn round_trip<T: Serialize + serde::de::DeserializeOwned>(json: &str) -> String {
        serde_json::to_string(&serde_json::from_str::<T>(json).unwrap()).unwrap()
    }

    #[rstest]
    #[case::parameter_null(round_trip::<Parameter>, r#"{"name":"q","in":"query","example":null}"#)]
    #[case::parameter_absent(round_trip::<Parameter>, r#"{"name":"q","in":"query"}"#)]
    #[case::media_type_null(round_trip::<MediaType>, r#"{"example":null}"#)]
    #[case::media_type_absent(round_trip::<MediaType>, "{}")]
    #[case::example_null(round_trip::<Example>, r#"{"value":null}"#)]
    #[case::example_absent(round_trip::<Example>, r#"{"summary":"s"}"#)]
    #[case::header_null(round_trip::<Header>, r#"{"example":null}"#)]
    #[case::header_absent(round_trip::<Header>, "{}")]
    #[case::nested_example_null(round_trip::<MediaType>, r#"{"examples":{"none":{"value":null}}}"#)]
    fn explicit_null_example_survives_round_trip(
        #[case] round_trip: fn(&str) -> String,
        #[case] json: &str,
    ) {
        assert_eq!(round_trip(json), json);
    }

    #[test]
    fn path_item_operations_iterates_present_methods_in_order() {
        let mut item = PathItem::default();
        assert_eq!(item.operations().count(), 0);
        for (method, id) in [(HttpMethod::Delete, "d"), (HttpMethod::Get, "g")] {
            item.set_operation(
                method,
                Operation {
                    operation_id: Some(id.to_string()),
                    ..Operation::default()
                },
            );
        }
        let seen: Vec<(HttpMethod, Option<&str>)> = item
            .operations()
            .map(|(m, op)| (m, op.operation_id.as_deref()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (HttpMethod::Get, Some("g")),
                (HttpMethod::Delete, Some("d"))
            ]
        );
    }

    #[rstest]
    #[case("GET", HttpMethod::Get)]
    #[case("get", HttpMethod::Get)]
    #[case("Get", HttpMethod::Get)]
    #[case("POST", HttpMethod::Post)]
    #[case("post", HttpMethod::Post)]
    #[case("Post", HttpMethod::Post)]
    #[case("PUT", HttpMethod::Put)]
    #[case("put", HttpMethod::Put)]
    #[case("Put", HttpMethod::Put)]
    #[case("PATCH", HttpMethod::Patch)]
    #[case("patch", HttpMethod::Patch)]
    #[case("Patch", HttpMethod::Patch)]
    #[case("DELETE", HttpMethod::Delete)]
    #[case("delete", HttpMethod::Delete)]
    #[case("Delete", HttpMethod::Delete)]
    #[case("HEAD", HttpMethod::Head)]
    #[case("head", HttpMethod::Head)]
    #[case("Head", HttpMethod::Head)]
    #[case("OPTIONS", HttpMethod::Options)]
    #[case("options", HttpMethod::Options)]
    #[case("Options", HttpMethod::Options)]
    #[case("TRACE", HttpMethod::Trace)]
    #[case("trace", HttpMethod::Trace)]
    #[case("Trace", HttpMethod::Trace)]
    fn test_http_method_from_str(#[case] input: &str, #[case] expected: HttpMethod) {
        let result = HttpMethod::try_from(input).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_http_method_from_invalid_str() {
        let result = HttpMethod::try_from("INVALID");
        assert!(result.is_err());
    }

    #[test]
    fn test_http_method_serialization() {
        // Test serde serialization (should be UPPERCASE)
        let method = HttpMethod::Get;
        let serialized = serde_json::to_string(&method).unwrap();
        assert_eq!(serialized, "\"GET\"");

        let method = HttpMethod::Post;
        let serialized = serde_json::to_string(&method).unwrap();
        assert_eq!(serialized, "\"POST\"");

        let method = HttpMethod::Delete;
        let serialized = serde_json::to_string(&method).unwrap();
        assert_eq!(serialized, "\"DELETE\"");
    }

    #[test]
    fn test_http_method_deserialization() {
        // Test serde deserialization
        let method: HttpMethod = serde_json::from_str("\"GET\"").unwrap();
        assert_eq!(method, HttpMethod::Get);

        let method: HttpMethod = serde_json::from_str("\"POST\"").unwrap();
        assert_eq!(method, HttpMethod::Post);

        let method: HttpMethod = serde_json::from_str("\"DELETE\"").unwrap();
        assert_eq!(method, HttpMethod::Delete);
    }

    #[test]
    fn test_path_item_set_operation() {
        let mut path_item = PathItem::default();

        let operation = Operation {
            operation_id: Some("test_operation".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
            deprecated: None,
            external_docs: None,
            callbacks: None,
            servers: None,
            extensions: crate::Extensions::new(),
        };

        // Test setting GET operation
        path_item.set_operation(HttpMethod::Get, operation.clone());
        assert!(path_item.get.is_some());
        assert_eq!(
            path_item.get.as_ref().unwrap().operation_id,
            Some("test_operation".to_string())
        );

        // Test setting POST operation
        let mut operation_post = operation.clone();
        operation_post.operation_id = Some("post_operation".to_string());
        path_item.set_operation(HttpMethod::Post, operation_post);
        assert!(path_item.post.is_some());
        assert_eq!(
            path_item.post.as_ref().unwrap().operation_id,
            Some("post_operation".to_string())
        );

        // Test setting PUT operation
        let mut operation_put = operation.clone();
        operation_put.operation_id = Some("put_operation".to_string());
        path_item.set_operation(HttpMethod::Put, operation_put);
        assert!(path_item.put.is_some());

        // Test setting PATCH operation
        let mut operation_patch = operation.clone();
        operation_patch.operation_id = Some("patch_operation".to_string());
        path_item.set_operation(HttpMethod::Patch, operation_patch);
        assert!(path_item.patch.is_some());

        // Test setting DELETE operation
        let mut operation_delete = operation.clone();
        operation_delete.operation_id = Some("delete_operation".to_string());
        path_item.set_operation(HttpMethod::Delete, operation_delete);
        assert!(path_item.delete.is_some());

        // Test setting HEAD operation
        let mut operation_head = operation.clone();
        operation_head.operation_id = Some("head_operation".to_string());
        path_item.set_operation(HttpMethod::Head, operation_head);
        assert!(path_item.head.is_some());

        // Test setting OPTIONS operation
        let mut operation_options = operation.clone();
        operation_options.operation_id = Some("options_operation".to_string());
        path_item.set_operation(HttpMethod::Options, operation_options);
        assert!(path_item.options.is_some());

        // Test setting TRACE operation
        let mut operation_trace = operation;
        operation_trace.operation_id = Some("trace_operation".to_string());
        path_item.set_operation(HttpMethod::Trace, operation_trace);
        assert!(path_item.trace.is_some());
    }

    #[test]
    fn test_path_item_set_operation_overwrites() {
        let mut path_item = PathItem::default();

        let operation1 = Operation {
            operation_id: Some("first".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
            deprecated: None,
            external_docs: None,
            callbacks: None,
            servers: None,
            extensions: crate::Extensions::new(),
        };

        let operation2 = Operation {
            operation_id: Some("second".to_string()),
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
            deprecated: None,
            external_docs: None,
            callbacks: None,
            servers: None,
            extensions: crate::Extensions::new(),
        };

        // Set first operation
        path_item.set_operation(HttpMethod::Get, operation1);
        assert_eq!(
            path_item.get.as_ref().unwrap().operation_id,
            Some("first".to_string())
        );

        // Overwrite with second operation
        path_item.set_operation(HttpMethod::Get, operation2);
        assert_eq!(
            path_item.get.as_ref().unwrap().operation_id,
            Some("second".to_string())
        );
    }

    #[test]
    fn operation_and_path_item_optional_openapi_fields_serialize_when_present() {
        let mut callbacks = BTreeMap::new();
        callbacks.insert(
            "onEvent".to_string(),
            Callback::from([(
                "{$request.body#/callbackUrl}".to_string(),
                PathItem::default(),
            )]),
        );
        let operation = Operation {
            operation_id: None,
            tags: None,
            summary: None,
            description: None,
            parameters: None,
            request_body: None,
            responses: BTreeMap::new(),
            security: None,
            deprecated: None,
            external_docs: None,
            callbacks: Some(callbacks),
            servers: Some(vec![Server {
                url: "https://api.example.com".to_string(),
                description: None,
                variables: None,
            }]),
            extensions: crate::Extensions::new(),
        };
        let path_item = PathItem {
            ref_path: Some("#/paths/~1users".to_string()),
            get: Some(operation),
            servers: Some(vec![Server {
                url: "https://path.example.com".to_string(),
                description: None,
                variables: None,
            }]),
            ..PathItem::default()
        };

        let json = serde_json::to_string(&path_item).unwrap();

        assert!(json.contains("\"$ref\":\"#/paths/~1users\""));
        assert!(json.contains("callbacks"));
        assert!(json.contains("servers"));
    }

    /// `callbacks` is name → (runtime expression → Path Item), as in the
    /// OpenAPI 3.1 Callback Object; the expression level must survive a
    /// round trip instead of being dropped as an unknown Path Item field.
    #[test]
    fn callbacks_nest_name_then_expression_then_path_item() {
        let json = serde_json::json!({
            "responses": { "200": { "description": "ok" } },
            "callbacks": {
                "onData": {
                    "{$request.query.callbackUrl}/data": {
                        "post": { "responses": { "202": { "description": "ok" } } }
                    }
                }
            }
        });
        let operation: Operation = serde_json::from_value(json.clone()).unwrap();
        let callback = &operation.callbacks.as_ref().unwrap()["onData"];
        let path_item: &PathItem = &callback["{$request.query.callbackUrl}/data"];
        assert!(path_item.post.is_some());
        assert_eq!(serde_json::to_value(&operation).unwrap(), json);
    }

    #[rstest]
    #[case(HttpMethod::Get, "GET")]
    #[case(HttpMethod::Post, "POST")]
    #[case(HttpMethod::Put, "PUT")]
    #[case(HttpMethod::Patch, "PATCH")]
    #[case(HttpMethod::Delete, "DELETE")]
    #[case(HttpMethod::Head, "HEAD")]
    #[case(HttpMethod::Options, "OPTIONS")]
    #[case(HttpMethod::Trace, "TRACE")]
    fn test_http_method_display(#[case] method: HttpMethod, #[case] expected: &str) {
        assert_eq!(method.to_string(), expected);
    }

    #[test]
    fn test_http_method_equality() {
        let method1 = HttpMethod::Get;
        let method2 = HttpMethod::Get;
        let method3 = HttpMethod::Post;

        assert_eq!(method1, method2);
        assert_ne!(method1, method3);
    }

    #[test]
    fn test_http_method_clone() {
        let method = HttpMethod::Get;
        let cloned = method;
        assert_eq!(method, cloned);
    }

    #[test]
    fn test_http_method_hash() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        map.insert(HttpMethod::Get, "GET method");
        map.insert(HttpMethod::Post, "POST method");

        assert_eq!(map.get(&HttpMethod::Get), Some(&"GET method"));
        assert_eq!(map.get(&HttpMethod::Post), Some(&"POST method"));
        assert_eq!(map.get(&HttpMethod::Put), None);
    }

    /// Round 13: an empty Responses Object is invalid OpenAPI, so an
    /// operation without responses omits the key instead of writing `{}`.
    #[test]
    fn operation_without_responses_omits_the_key() {
        let operation = Operation {
            summary: Some("pet created".to_owned()),
            ..Operation::default()
        };
        assert_eq!(
            serde_json::to_value(&operation).unwrap(),
            serde_json::json!({"summary": "pet created"})
        );
    }

    /// The documented limitation (module docs, "Reference Objects"): a
    /// Reference Object outside a schema position does not deserialize.
    #[test]
    fn reference_object_outside_a_schema_is_rejected() {
        let error = serde_json::from_value::<Operation>(serde_json::json!({
            "responses": {"200": {"$ref": "#/components/responses/Ok"}}
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("missing field `description`"),
            "{error}"
        );
    }

    /// `Header` / `Example` have no required field, so without the `$ref`
    /// guard a reference would silently become `{}`.
    #[rstest]
    #[case::header(serde_json::json!({"headers": {"X-Rate": {"$ref": "#/components/headers/Rate"}}}), "Header")]
    #[case::example(serde_json::json!({"content": {"application/json": {"examples": {"ok": {"$ref": "#/components/examples/Ok"}}}}}), "Example")]
    #[case::header_example(serde_json::json!({"headers": {"X-Rate": {"examples": {"ok": {"$ref": "#/components/examples/Ok"}}}}}), "Example")]
    fn reference_object_in_an_all_optional_position_is_rejected(
        #[case] mut response: serde_json::Value,
        #[case] what: &str,
    ) {
        response["description"] = "ok".into();
        let error = serde_json::from_value::<Response>(response)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!(
                "a Reference Object (`$ref`) is not supported as a {what} Object"
            )),
            "{error}"
        );
    }

    #[test]
    fn inline_header_and_example_round_trip() {
        let json = serde_json::json!({
            "description": "ok",
            "headers": {"X-Rate": {"description": "limit", "schema": {"type": "integer"}, "examples": {"one": {"summary": "s", "value": 1}}}},
            "content": {"application/json": {"examples": {"ok": {"externalValue": "https://e.example/ok.json"}}}}
        });
        let response: Response = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&response).unwrap(), json);
        let error =
            serde_json::from_value::<Header>(serde_json::json!({"required": "yes"})).unwrap_err();
        assert!(error.to_string().contains("invalid type"), "{error}");
    }
}
