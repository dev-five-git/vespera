use super::Schema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `OpenAPI` Components (reusable components)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Components {
    /// Schema definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schemas: Option<BTreeMap<String, Schema>>,
    /// Response definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses: Option<BTreeMap<String, crate::route::Response>>,
    /// Parameter definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<BTreeMap<String, crate::route::Parameter>>,
    /// Example definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<BTreeMap<String, crate::route::Example>>,
    /// Request body definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_bodies: Option<BTreeMap<String, crate::route::RequestBody>>,
    /// Header definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, crate::route::Header>>,
    /// Security scheme definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<BTreeMap<String, SecurityScheme>>,
}

/// Security scheme type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecuritySchemeType {
    ApiKey,
    Http,
    /// OpenAPI's canonical wire name is `mutualTLS` (not the `camelCase`
    /// `mutualTls` the container rule would produce).
    #[serde(rename = "mutualTLS")]
    MutualTls,
    /// OpenAPI's canonical wire name is `oauth2`; the `camelCase` container
    /// rule would otherwise lowercase only the leading char and emit the
    /// invalid `oAuth2`.
    #[serde(rename = "oauth2")]
    OAuth2,
    OpenIdConnect,
}

/// Security scheme definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityScheme {
    /// Security scheme type
    pub r#type: SecuritySchemeType,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Name (for API Key)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Location (for API Key: query, header, cookie)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#in: Option<String>,
    /// Scheme (for HTTP: bearer, basic, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// Bearer format (for HTTP Bearer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_format: Option<String>,
    /// OAuth2 flows (for OAuth2 security schemes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flows: Option<OAuthFlows>,
    /// OpenID Connect discovery URL (for OpenID Connect security schemes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id_connect_url: Option<String>,
}

/// OAuth2 flow definitions for OpenAPI security schemes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlows {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implicit: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<OAuthFlow>,
}

/// OAuth2 flow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    pub scopes: BTreeMap<String, String>,
}
