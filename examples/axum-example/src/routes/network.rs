//! std / `url` types that serde writes as JSON strings are OpenAPI builtins:
//! no `#[derive(Schema)]` (impossible under the orphan rule) is needed.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use url::Url;
use vespera::{Schema, axum::Json};

/// Network endpoint description built from std / `url` types.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct NetworkEndpoint {
    /// Either address family — `string`.
    pub ip: IpAddr,
    /// `string` / `ipv4`.
    pub ipv4: Ipv4Addr,
    /// `string` / `ipv6`.
    pub ipv6: Ipv6Addr,
    /// `ip:port` — `string`.
    pub bind: SocketAddr,
    /// Filesystem path — `string`.
    pub data_dir: PathBuf,
    /// `string` / `uri`.
    pub homepage: Url,
    /// Optional callback URL.
    pub callback: Option<Url>,
    /// `{"secs", "nanos"}` object (serde's std `Duration` shape).
    pub timeout: std::time::Duration,
}

/// Sample endpoint.
#[vespera::route(get, tags = ["network"])]
pub async fn get_network_endpoint() -> Json<NetworkEndpoint> {
    Json(NetworkEndpoint {
        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ipv4: Ipv4Addr::new(10, 0, 0, 1),
        ipv6: Ipv6Addr::LOCALHOST,
        bind: SocketAddr::from(([0, 0, 0, 0], 3000)),
        data_dir: PathBuf::from("/var/lib/app"),
        homepage: Url::parse("https://example.com/").unwrap(),
        callback: None,
        timeout: Duration::from_millis(1500),
    })
}

/// Echo an endpoint back.
#[vespera::route(post, tags = ["network"])]
pub async fn echo_network_endpoint(Json(endpoint): Json<NetworkEndpoint>) -> Json<NetworkEndpoint> {
    Json(endpoint)
}
