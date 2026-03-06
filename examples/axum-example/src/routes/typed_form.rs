use serde::Serialize;
use tempfile::NamedTempFile;
use vespera::axum::Json;
use vespera::axum::http::StatusCode;
use vespera::multipart::{FieldData, TypedMultipart};
use vespera::{Multipart, Schema, route};

// ============== Request/Response DTOs ==============

#[derive(Debug, Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadResponse {
    pub id: i64,
    pub name: String,
    pub thumbnail_url: Option<String>,
    pub document_url: Option<String>,
    pub tags: Vec<String>,
    pub is_active: bool,
    pub created_at: String,
}

#[derive(Debug, Multipart, Schema)]
pub struct CreateFileUploadRequest {
    pub name: String,
    #[form_data(limit = "10MiB")]
    pub thumbnail: Option<FieldData<NamedTempFile>>,
    #[form_data(limit = "50MiB")]
    pub document: Option<FieldData<NamedTempFile>>,
    pub tags: Option<String>,
}

#[derive(Debug, Multipart, Schema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFileUploadRequest {
    pub name: Option<String>,
    #[form_data(limit = "10MiB")]
    pub thumbnail: Option<FieldData<NamedTempFile>>,
    #[form_data(limit = "50MiB")]
    pub document: Option<FieldData<NamedTempFile>>,
    pub tags: Option<String>,
    pub is_active: Option<bool>,
}

// Generated via schema_type! with multipart: derives Multipart + Schema,
// partial makes all fields Option, omits the "document" field, preserves form_data attrs.
// Note: multipart automatically sets clone = false (FieldData<NamedTempFile> doesn't implement Clone).
vespera::schema_type!(PatchFileUploadRequest from UpdateFileUploadRequest, multipart, partial, omit = ["document"]);

// ============== Handlers ==============

/// List all file uploads
#[route(get, tags = ["typed-form"])]
pub async fn list_file_uploads() -> Json<Vec<FileUploadResponse>> {
    Json(vec![FileUploadResponse {
        id: 1,
        name: "Sample Upload".to_string(),
        thumbnail_url: Some("https://example.com/thumb.jpg".to_string()),
        document_url: Some("https://example.com/doc.pdf".to_string()),
        tags: vec!["sample".to_string(), "test".to_string()],
        is_active: true,
        created_at: "2024-01-01T00:00:00Z".to_string(),
    }])
}

/// Create a new file upload with multipart form data
#[route(post, tags = ["typed-form"])]
pub async fn create_file_upload(
    TypedMultipart(req): TypedMultipart<CreateFileUploadRequest>,
) -> Result<Json<FileUploadResponse>, (StatusCode, String)> {
    let tags: Vec<String> = req
        .tags
        .map(|t| {
            t.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(FileUploadResponse {
        id: 1,
        name: req.name,
        thumbnail_url: req.thumbnail.map(|_| "uploaded_thumbnail_url".to_string()),
        document_url: req.document.map(|_| "uploaded_document_url".to_string()),
        tags,
        is_active: true,
        created_at: "2024-01-01T00:00:00Z".to_string(),
    }))
}

/// Update a file upload with multipart form data
#[route(put, path = "/{id}", tags = ["typed-form"])]
pub async fn update_file_upload(
    vespera::axum::extract::Path(id): vespera::axum::extract::Path<i64>,
    TypedMultipart(req): TypedMultipart<UpdateFileUploadRequest>,
) -> Result<Json<FileUploadResponse>, (StatusCode, String)> {
    Ok(Json(FileUploadResponse {
        id,
        name: req.name.unwrap_or_else(|| "Unchanged".to_string()),
        thumbnail_url: req.thumbnail.map(|_| "updated_thumbnail_url".to_string()),
        document_url: req.document.map(|_| "updated_document_url".to_string()),
        tags: req
            .tags
            .map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        is_active: req.is_active.unwrap_or(true),
        created_at: "2024-01-01T00:00:00Z".to_string(),
    }))
}

/// Patch a file upload (partial update via schema_type! multipart)
#[route(patch, path = "/{id}", tags = ["typed-form"])]
pub async fn patch_file_upload(
    vespera::axum::extract::Path(id): vespera::axum::extract::Path<i64>,
    TypedMultipart(req): TypedMultipart<PatchFileUploadRequest>,
) -> Result<Json<FileUploadResponse>, (StatusCode, String)> {
    Ok(Json(FileUploadResponse {
        id,
        name: req.name.unwrap_or_else(|| "Unchanged".to_string()),
        thumbnail_url: req.thumbnail.map(|_| "patched_thumbnail_url".to_string()),
        document_url: None,
        tags: req
            .tags
            .map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        is_active: true,
        created_at: "2024-01-01T00:00:00Z".to_string(),
    }))
}
