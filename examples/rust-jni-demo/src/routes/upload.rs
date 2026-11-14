//! Multipart upload route — the JNI E2E suite posts files through the Spring
//! proxy. Spring Boot's multipart resolver would parse (and drain) the body
//! before the proxy runs and reject files over its 1 MB `max-file-size`; the
//! bridge's `VesperaProxyMultipartResolver` keeps it off proxied requests, so
//! the raw body and boundary reach this extractor byte-for-byte.

use serde::Serialize;
use vespera::{
    Schema,
    axum::{Json, extract::Multipart, http::StatusCode},
};

/// One received multipart part.
#[derive(Debug, Clone, Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct UploadedPart {
    pub name: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub size: u64,
    /// FNV-1a 64-bit hash of the part bytes (16 lower-case hex digits).
    pub fnv1a: String,
}

/// Every part of a multipart upload, in order.
#[derive(Debug, Clone, Serialize, Schema)]
pub struct UploadSummary {
    pub parts: Vec<UploadedPart>,
}

/// Summarise a multipart upload (name, file name, content type, size and
/// hash of every part).
#[vespera::route(post, tags = ["upload"])]
pub async fn upload(mut multipart: Multipart) -> Result<Json<UploadSummary>, (StatusCode, String)> {
    let bad_request = |error: vespera::axum::extract::multipart::MultipartError| {
        (StatusCode::BAD_REQUEST, error.body_text())
    };
    let mut parts = Vec::new();
    while let Some(field) = multipart.next_field().await.map_err(bad_request)? {
        let name = field.name().unwrap_or_default().to_owned();
        let file_name = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(str::to_owned);
        let bytes = field.bytes().await.map_err(bad_request)?;
        parts.push(UploadedPart {
            name,
            file_name,
            content_type,
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            fnv1a: format!("{:016x}", fnv1a(&bytes)),
        });
    }
    Ok(Json(UploadSummary { parts }))
}

/// FNV-1a 64-bit (the Java E2E test computes the same hash).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::fnv1a;

    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
