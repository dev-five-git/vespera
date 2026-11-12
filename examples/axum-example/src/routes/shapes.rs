//! Request/response shapes whose OpenAPI documentation follows the real
//! wire format: struct path params, wildcard segments, query flatten/skip and
//! repeated keys, opaque / HTML / binary bodies, optional JSON bodies, any
//! JSON values, std durations, enum `rename_all_fields`, `deny_unknown_fields`
//! and flattened `Option`s.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{
        Json,
        body::Bytes,
        extract::Path,
        http::header,
        response::{Html, IntoResponse, Redirect},
    },
    axum_extra::extract::Query,
};

/// Path segments typed by field name.
#[derive(Deserialize, Schema)]
pub struct ItemPath {
    pub item_id: u32,
    pub slug: String,
}

/// `GET /shapes/items/{item_id}/{slug}` — `item_id` is documented as an
/// integer and `slug` as a string (not the whole struct).
#[vespera::route(get, path = "/items/{item_id}/{slug}")]
pub async fn item_by_path(Path(path): Path<ItemPath>) -> String {
    format!("{}:{}", path.item_id, path.slug)
}

/// `GET /shapes/files/{*rest}` — documented as `/shapes/files/{rest}`.
#[vespera::route(get, path = "/files/{*rest}")]
pub async fn file_by_wildcard(Path(rest): Path<String>) -> String {
    rest
}

/// Paging parameters flattened into a query string (strings only: serde
/// buffers flattened values, so typed numbers would not parse from a query).
#[derive(Deserialize, Schema)]
pub struct Paging {
    pub cursor: Option<String>,
}

/// Query string: `tags` repeats (`?tags=a&tags=b`, `style: form, explode:
/// true`), `internal` is never read, `Paging`'s fields are inlined.
#[derive(Deserialize, Schema)]
pub struct SearchQuery {
    pub q: String,
    pub tags: Option<Vec<String>>,
    #[serde(skip)]
    pub internal: u8,
    #[serde(flatten)]
    pub paging: Paging,
}

/// `GET /shapes/search` (axum-extra `Query` deserializes repeated keys).
#[vespera::route(get, path = "/search")]
pub async fn search(Query(query): Query<SearchQuery>) -> String {
    format!(
        "{}|{}|{}|{}",
        query.q,
        query.tags.unwrap_or_default().join(","),
        query.internal,
        query.paging.cursor.unwrap_or_default()
    )
}

/// `text/html` response.
#[vespera::route(get, path = "/html")]
pub async fn html_page() -> Html<&'static str> {
    Html("<h1>vespera</h1>")
}

/// `application/octet-stream` response behind a header array.
#[vespera::route(get, path = "/bytes")]
pub async fn raw_bytes() -> ([(header::HeaderName, &'static str); 1], Vec<u8>) {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        vec![0, 1, 2],
    )
}

/// Redirects carry no documented body.
#[vespera::route(get, path = "/redirect")]
pub async fn redirect() -> Redirect {
    Redirect::temporary("/health")
}

/// `impl IntoResponse` is opaque: description only.
#[vespera::route(get, path = "/opaque")]
pub async fn opaque() -> impl IntoResponse {
    "opaque"
}

/// `application/octet-stream` request body.
#[vespera::route(post, path = "/upload")]
pub async fn upload_bytes(body: Bytes) -> Json<usize> {
    Json(body.len())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct Note {
    pub text: String,
}

/// Optional JSON body (`requestBody.required: false`).
#[vespera::route(post, path = "/note")]
pub async fn optional_note(body: Option<Json<Note>>) -> String {
    body.map_or_else(|| "none".to_string(), |Json(note)| note.text)
}

/// An enum whose struct-variant fields are camelCased while the variants
/// themselves are snake_cased.
#[derive(Serialize, Deserialize, Schema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ShapeEvent {
    JobStarted { job_id: u32 },
    JobFinished { job_id: u32, exit_code: i32 },
}

/// Optional audit block flattened into [`Telemetry`].
#[derive(Serialize, Deserialize, Schema)]
pub struct Audit {
    pub actor: String,
}

/// Any-JSON payload, a std `Duration` (`{secs, nanos}`) and an optional
/// flattened block (its fields are not required).
#[derive(Serialize, Deserialize, Schema)]
pub struct Telemetry {
    pub payload: serde_json::Value,
    pub elapsed: Duration,
    pub event: ShapeEvent,
    #[serde(flatten)]
    pub audit: Option<Audit>,
}

/// A request object that rejects unknown fields (`additionalProperties: false`).
#[derive(Serialize, Deserialize, Schema)]
#[serde(deny_unknown_fields)]
pub struct StrictInput {
    pub name: String,
}

#[vespera::route(post, path = "/telemetry")]
pub async fn telemetry(Json(input): Json<StrictInput>) -> Json<Telemetry> {
    Json(Telemetry {
        payload: serde_json::json!({ "name": input.name }),
        elapsed: Duration::from_millis(1500),
        event: ShapeEvent::JobFinished {
            job_id: 7,
            exit_code: 0,
        },
        audit: None,
    })
}

/// Explicit paths are served verbatim (no kebab-casing): `/shapes/raw_path`.
#[vespera::route(get, path = "/raw_path")]
pub async fn raw_path() -> &'static str {
    "raw"
}
