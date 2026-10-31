//! In-process HTTP client for the examples' integration tests.
//!
//! Drives an [`axum::Router`](vespera::axum::Router) through
//! `tower::ServiceExt::oneshot` - no socket, no extra crates beyond the ones
//! axum already depends on. It covers exactly the request/response surface the
//! examples' tests use.

use std::future::{Future, IntoFuture};
use std::pin::Pin;

use serde::{Serialize, de::DeserializeOwned};
use tower::ServiceExt;
use vespera::axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header::CONTENT_TYPE},
};

const MULTIPART_BOUNDARY: &str = "----testkit-boundary";

/// A router wrapped for issuing requests against it.
pub struct TestServer {
    router: Router,
}

impl TestServer {
    #[must_use]
    pub const fn new(router: Router) -> Self {
        Self { router }
    }

    #[must_use]
    pub fn get(&self, path: &str) -> TestRequest {
        self.request(Method::GET, path)
    }

    #[must_use]
    pub fn post(&self, path: &str) -> TestRequest {
        self.request(Method::POST, path)
    }

    #[must_use]
    pub fn put(&self, path: &str) -> TestRequest {
        self.request(Method::PUT, path)
    }

    #[must_use]
    pub fn patch(&self, path: &str) -> TestRequest {
        self.request(Method::PATCH, path)
    }

    fn request(&self, method: Method, path: &str) -> TestRequest {
        TestRequest {
            router: self.router.clone(),
            method,
            uri: path.to_owned(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }
}

/// A pending request; `.await` sends it and yields the [`TestResponse`].
pub struct TestRequest {
    router: Router,
    method: Method,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

impl TestRequest {
    #[must_use]
    pub fn json<T: Serialize>(self, value: &T) -> Self {
        let body = serde_json::to_vec(value).expect("request body serializes to JSON");
        self.content_type("application/json").bytes(body.into())
    }

    #[must_use]
    pub fn multipart(self, form: MultipartForm) -> Self {
        self.content_type(&format!(
            "multipart/form-data; boundary={MULTIPART_BOUNDARY}"
        ))
        .bytes(form.encode().into())
    }

    #[must_use]
    pub fn content_type(self, content_type: &str) -> Self {
        self.add_header(CONTENT_TYPE, content_type)
    }

    #[must_use]
    pub fn bytes(mut self, body: Bytes) -> Self {
        self.body = body;
        self
    }

    #[must_use]
    pub fn add_header(mut self, name: HeaderName, value: &str) -> Self {
        self.headers.insert(
            name,
            HeaderValue::from_str(value).expect("valid header value"),
        );
        self
    }
}

impl IntoFuture for TestRequest {
    type Output = TestResponse;
    type IntoFuture = Pin<Box<dyn Future<Output = TestResponse> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let mut request = Request::builder()
                .method(self.method)
                .uri(self.uri)
                .body(Body::from(self.body))
                .expect("valid request");
            *request.headers_mut() = self.headers;
            let Ok(response) = self.router.oneshot(request).await;
            let (parts, body) = response.into_parts();
            TestResponse {
                status: parts.status,
                headers: parts.headers,
                body: to_bytes(body, usize::MAX)
                    .await
                    .expect("response body is readable"),
            }
        })
    }
}

/// A fully buffered response.
pub struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl TestResponse {
    #[must_use]
    pub const fn status_code(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub const fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    #[must_use]
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).expect("response body is the expected JSON")
    }

    pub fn assert_status(&self, expected: StatusCode) {
        assert_eq!(self.status, expected, "response body: {}", self.text());
    }

    pub fn assert_status_ok(&self) {
        self.assert_status(StatusCode::OK);
    }

    pub fn assert_status_not_found(&self) {
        self.assert_status(StatusCode::NOT_FOUND);
    }

    pub fn assert_text(&self, expected: &str) {
        assert_eq!(self.text(), expected);
    }
}

/// A `multipart/form-data` body under construction.
#[derive(Default)]
pub struct MultipartForm {
    parts: Vec<(String, Part)>,
}

impl MultipartForm {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn add_text(self, name: &str, text: impl Into<String>) -> Self {
        self.add_part(name, Part::bytes(text.into().into_bytes()))
    }

    #[must_use]
    pub fn add_part(mut self, name: &str, part: Part) -> Self {
        self.parts.push((name.to_owned(), part));
        self
    }

    fn encode(self) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, part) in self.parts {
            body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"").as_bytes(),
            );
            if let Some(file_name) = part.file_name {
                body.extend_from_slice(format!("; filename=\"{file_name}\"").as_bytes());
            }
            body.extend_from_slice(b"\r\n\r\n");
            body.extend_from_slice(&part.bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
        body
    }
}

/// One field of a [`MultipartForm`]; a file when it has a file name.
pub struct Part {
    bytes: Vec<u8>,
    file_name: Option<String>,
}

impl Part {
    #[must_use]
    pub fn bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            file_name: None,
        }
    }

    #[must_use]
    pub fn file_name(mut self, file_name: &str) -> Self {
        self.file_name = Some(file_name.to_owned());
        self
    }
}
