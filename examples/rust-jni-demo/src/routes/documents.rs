//! Document validation routes — the demo domain.
//!
//! Uses `#[vespera::route]` and `#[derive(Schema)]` so that routes
//! are auto-discovered by `vespera!()` and DTOs appear in the OpenAPI
//! spec, exactly like `axum-example/src/routes/users.rs`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use vespera::{Schema, axum::Json};

// ── DTOs (with Schema for OpenAPI) ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ValidateDocumentRequest {
    pub document_type: String,
    pub title: String,
    pub content: String,
    pub author: String,
    pub department: String,
    pub classification: String,
    pub effective_date: String,
    #[serde(default)]
    pub expiry_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ValidationResult {
    pub valid: bool,
    pub document_id: String,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
    pub metadata: DocumentMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ValidationError {
    pub code: String,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ValidationWarning {
    pub code: String,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMetadata {
    pub word_count: usize,
    pub char_count: usize,
    pub estimated_pages: usize,
    pub classification_level: u8,
    pub retention_years: u32,
}

// ── Route Handlers ───────────────────────────────────────────────────

/// Validate a document against the organisation's rule set
#[allow(clippy::unused_async)]
#[vespera::route(post, path = "/validate")]
pub async fn validate(Json(req): Json<ValidateDocumentRequest>) -> Json<ValidationResult> {
    Json(validate_document(&req))
}

// ── Business Logic ───────────────────────────────────────────────────

const VALID_DOC_TYPES: &[&str] = &["regulation", "policy", "memo", "report", "contract"];
const VALID_CLASSIFICATIONS: &[&str] = &["public", "internal", "confidential", "secret"];
const MAX_TITLE_LEN: usize = 200;
const MIN_WORD_COUNT: usize = 10;

#[allow(clippy::too_many_lines)]
fn validate_document(req: &ValidateDocumentRequest) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if !VALID_DOC_TYPES.contains(&req.document_type.as_str()) {
        errors.push(ValidationError {
            code: "DOC_TYPE_INVALID".into(),
            field: "documentType".into(),
            message: format!(
                "Invalid document type '{}'. Must be one of: {}",
                req.document_type,
                VALID_DOC_TYPES.join(", ")
            ),
        });
    }

    if req.title.trim().is_empty() {
        errors.push(ValidationError {
            code: "TITLE_EMPTY".into(),
            field: "title".into(),
            message: "Document title is required".into(),
        });
    } else if req.title.len() > MAX_TITLE_LEN {
        errors.push(ValidationError {
            code: "TITLE_TOO_LONG".into(),
            field: "title".into(),
            message: format!(
                "Title exceeds {MAX_TITLE_LEN} characters (got {})",
                req.title.len()
            ),
        });
    }

    if req.content.trim().is_empty() {
        errors.push(ValidationError {
            code: "CONTENT_EMPTY".into(),
            field: "content".into(),
            message: "Document content is required".into(),
        });
    }

    let word_count = req.content.split_whitespace().count();
    if word_count > 0 && word_count < MIN_WORD_COUNT {
        warnings.push(ValidationWarning { code: "CONTENT_SHORT".into(), field: "content".into(), message: format!("Document content is very short ({word_count} words, recommended minimum: {MIN_WORD_COUNT})") });
    }

    let level = classification_to_level(&req.classification);
    if level.is_none() {
        errors.push(ValidationError {
            code: "CLASSIFICATION_INVALID".into(),
            field: "classification".into(),
            message: format!(
                "Invalid classification '{}'. Must be one of: {}",
                req.classification,
                VALID_CLASSIFICATIONS.join(", ")
            ),
        });
    }
    let level = level.unwrap_or(0);

    if level >= 2 {
        if req.author.trim().is_empty() {
            errors.push(ValidationError {
                code: "AUTHOR_REQUIRED_FOR_CLASSIFIED".into(),
                field: "author".into(),
                message: "Author is required for confidential/secret documents".into(),
            });
        }
        if req.department.trim().is_empty() {
            errors.push(ValidationError {
                code: "DEPT_REQUIRED_FOR_CLASSIFIED".into(),
                field: "department".into(),
                message: "Department is required for confidential/secret documents".into(),
            });
        }
    }

    let eff_ok = parse_date(&req.effective_date).is_some();
    if !eff_ok {
        errors.push(ValidationError {
            code: "DATE_FORMAT_INVALID".into(),
            field: "effectiveDate".into(),
            message: "effectiveDate must be YYYY-MM-DD".into(),
        });
    }

    if let Some(ref expiry) = req.expiry_date {
        let exp_ok = parse_date(expiry).is_some();
        if !exp_ok {
            errors.push(ValidationError {
                code: "DATE_FORMAT_INVALID".into(),
                field: "expiryDate".into(),
                message: "expiryDate must be YYYY-MM-DD".into(),
            });
        }
        if eff_ok && exp_ok {
            let eff = parse_date(&req.effective_date).unwrap();
            let exp = parse_date(expiry).unwrap();
            if exp <= eff {
                errors.push(ValidationError {
                    code: "DATE_RANGE_INVALID".into(),
                    field: "expiryDate".into(),
                    message: "expiryDate must be after effectiveDate".into(),
                });
            }
        }
    }

    if req.document_type == "contract" && req.expiry_date.is_none() {
        warnings.push(ValidationWarning {
            code: "CONTRACT_NO_EXPIRY".into(),
            field: "expiryDate".into(),
            message: "Contracts should specify an expiry date".into(),
        });
    }

    if req.document_type == "regulation" && level < 1 {
        warnings.push(ValidationWarning {
            code: "REGULATION_PUBLIC".into(),
            field: "classification".into(),
            message: "Regulations are typically classified 'internal' or higher".into(),
        });
    }

    let char_count = req.content.chars().count();
    let estimated_pages = (word_count / 250).max(1);

    ValidationResult {
        valid: errors.is_empty(),
        document_id: generate_document_id(),
        errors,
        warnings,
        metadata: DocumentMetadata {
            word_count,
            char_count,
            estimated_pages,
            classification_level: level,
            retention_years: retention_for_level(level),
        },
    }
}

fn classification_to_level(c: &str) -> Option<u8> {
    match c {
        "public" => Some(0),
        "internal" => Some(1),
        "confidential" => Some(2),
        "secret" => Some(3),
        _ => None,
    }
}

fn retention_for_level(level: u8) -> u32 {
    match level {
        0 => 1,
        1 => 3,
        2 => 10,
        _ => 30,
    }
}

fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1 {
        return None;
    }
    Some((year, month, day))
}

fn generate_document_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("DOC-{ts}")
}
