use axum_example::create_app;
use axum_test::{
    TestServer,
    multipart::{MultipartForm, Part},
};

// Tests for TypedMultipart (Multipart) request body extraction

#[tokio::test]
async fn test_typed_form_list_file_uploads() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let response = server.get("/typed-form").await;

    response.assert_status_ok();
    let uploads: serde_json::Value = response.json();

    assert!(uploads.is_array());
    let uploads = uploads.as_array().unwrap();
    assert_eq!(uploads.len(), 1);

    let upload = &uploads[0];
    assert_eq!(upload["id"], 1);
    assert_eq!(upload["name"], "Sample Upload");
    assert_eq!(upload["thumbnailUrl"], "https://example.com/thumb.jpg");
    assert_eq!(upload["documentUrl"], "https://example.com/doc.pdf");
    assert_eq!(upload["isActive"], true);
    assert!(upload["tags"].is_array());
    assert_eq!(upload["tags"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_typed_form_create_file_upload() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let form = MultipartForm::new()
        .add_text("name", "Test Upload")
        .add_text("tags", "rust, axum, vespera");

    let response = server.post("/typed-form").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 1);
    assert_eq!(result["name"], "Test Upload");
    assert_eq!(result["isActive"], true);

    // Tags should be parsed from comma-separated string
    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 3);
    assert_eq!(tags[0], "rust");
    assert_eq!(tags[1], "axum");
    assert_eq!(tags[2], "vespera");

    // No files uploaded, so URLs should be null
    assert!(result["thumbnailUrl"].is_null());
    assert!(result["documentUrl"].is_null());
}

#[tokio::test]
async fn test_typed_form_create_file_upload_with_files() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let thumbnail_part = Part::bytes(b"fake image data".as_slice()).file_name("thumb.jpg");
    let document_part = Part::bytes(b"fake pdf data".as_slice()).file_name("doc.pdf");

    let form = MultipartForm::new()
        .add_text("name", "Upload With Files")
        .add_part("thumbnail", thumbnail_part)
        .add_part("document", document_part)
        .add_text("tags", "files");

    let response = server.post("/typed-form").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["name"], "Upload With Files");
    assert_eq!(result["thumbnailUrl"], "uploaded_thumbnail_url");
    assert_eq!(result["documentUrl"], "uploaded_document_url");

    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0], "files");
}

#[tokio::test]
async fn test_typed_form_update_file_upload() {
    let app = create_app().await;
    let server = TestServer::new(app);

    // UpdateFileUploadRequest has #[serde(rename_all = "camelCase")] which vespera::Multipart respects.
    // The Multipart derive reads serde attrs to match field names at runtime.
    let form = MultipartForm::new()
        .add_text("name", "Updated Upload")
        .add_text("tags", "updated, tags")
        .add_text("isActive", "false");

    let response = server.put("/typed-form/42").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 42);
    assert_eq!(result["name"], "Updated Upload");
    // Response uses FileUploadResponse with #[serde(rename_all = "camelCase")]
    assert_eq!(result["isActive"], false);

    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0], "updated");
    assert_eq!(tags[1], "tags");
}

#[tokio::test]
async fn test_typed_form_update_file_upload_with_file() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let thumbnail_part = Part::bytes(b"new image".as_slice()).file_name("new_thumb.jpg");

    let form = MultipartForm::new()
        .add_text("name", "Updated With File")
        .add_part("thumbnail", thumbnail_part);

    let response = server.put("/typed-form/7").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 7);
    assert_eq!(result["name"], "Updated With File");
    assert_eq!(result["thumbnailUrl"], "updated_thumbnail_url");
    // Document not provided in this update
    assert!(result["documentUrl"].is_null());
}

#[tokio::test]
async fn test_typed_form_patch_file_upload() {
    let app = create_app().await;
    let server = TestServer::new(app);

    // PatchFileUploadRequest is generated via schema_type! with multipart + partial + omit = ["document"]
    // All fields are Option<T>, so we only send the ones we want to update
    let form = MultipartForm::new().add_text("name", "Patched Upload");

    let response = server.patch("/typed-form/99").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 99);
    assert_eq!(result["name"], "Patched Upload");
    // document_url is always None for patch (field omitted from PatchFileUploadRequest)
    assert!(result["documentUrl"].is_null());
}

#[tokio::test]
async fn test_typed_form_patch_file_upload_with_thumbnail() {
    let app = create_app().await;
    let server = TestServer::new(app);

    let thumbnail_part = Part::bytes(b"patched image".as_slice()).file_name("patched.jpg");

    let form = MultipartForm::new()
        .add_text("name", "Patched With Thumb")
        .add_part("thumbnail", thumbnail_part)
        .add_text("tags", "patched");

    let response = server.patch("/typed-form/55").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["id"], 55);
    assert_eq!(result["name"], "Patched With Thumb");
    assert_eq!(result["thumbnailUrl"], "patched_thumbnail_url");
    assert!(result["documentUrl"].is_null());

    let tags = result["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0], "patched");
}

#[tokio::test]
async fn test_typed_form_create_minimal() {
    let app = create_app().await;
    let server = TestServer::new(app);

    // Only required field is "name" — all others are optional
    let form = MultipartForm::new().add_text("name", "Minimal Upload");

    let response = server.post("/typed-form").multipart(form).await;

    response.assert_status_ok();
    let result: serde_json::Value = response.json();

    assert_eq!(result["name"], "Minimal Upload");
    assert!(result["thumbnailUrl"].is_null());
    assert!(result["documentUrl"].is_null());
    assert!(result["tags"].as_array().unwrap().is_empty());
}
