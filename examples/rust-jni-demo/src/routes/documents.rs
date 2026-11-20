//! Document validation routes — the demo domain.
//!
//! Uses `#[vespera::route]` and `#[derive(Schema)]` so that routes
//! are auto-discovered by `vespera!()` and DTOs appear in the OpenAPI
//! spec, exactly like `axum-example/src/routes/users.rs`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use vespera::{Schema, Validated, axum::Json};

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
    /// `None` for an unrecognised classification (it has no level).
    pub classification_level: Option<u8>,
    /// `None` exactly when `classification_level` is (no retention is known).
    pub retention_years: Option<u32>,
}

/// Register request — `#[schema]` rules drive both the OpenAPI spec and the
/// runtime `garde::Validate` impl behind `Validated<T>` (422 on failure).
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct RegisterDocumentRequest {
    #[schema(min_length = 1, max_length = 200)]
    pub title: String,
    #[schema(minimum = 1, maximum = 9999)]
    pub page_count: u32,
}

// ── Route Handlers ───────────────────────────────────────────────────

/// Validate a document against the organisation's rule set
#[allow(clippy::unused_async)]
#[vespera::route(post, path = "/validate")]
pub async fn validate(Json(req): Json<ValidateDocumentRequest>) -> Json<ValidationResult> {
    Json(validate_document(&req))
}

/// Register a document; an invalid body is answered `422` with the
/// `{"errors":[...]}` envelope (hoisted into the wire's `validation_errors`).
#[allow(clippy::unused_async)]
#[vespera::route(post, path = "/register")]
pub async fn register(
    Validated(Json(req)): Validated<Json<RegisterDocumentRequest>>,
) -> Json<RegisterDocumentRequest> {
    Json(req)
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

    let title_chars = req.title.chars().count();
    if req.title.trim().is_empty() {
        errors.push(ValidationError {
            code: "TITLE_EMPTY".into(),
            field: "title".into(),
            message: "Document title is required".into(),
        });
    } else if title_chars > MAX_TITLE_LEN {
        errors.push(ValidationError {
            code: "TITLE_TOO_LONG".into(),
            field: "title".into(),
            message: format!("Title exceeds {MAX_TITLE_LEN} characters (got {title_chars})"),
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
    // An unrecognised classification is not known to be public, so it gets the
    // stricter rules of a classified document (author and department).
    if level.is_none_or(|level| level >= 2) {
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

    if req.document_type == "regulation" && level == Some(0) {
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
            retention_years: level.map(retention_for_level),
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
    // Exactly `YYYY-MM-DD`: `str::parse` alone would also take `+1` and `1`.
    if parts.len() != 3
        || !is_digits(parts[0], 4)
        || !is_digits(parts[1], 2)
        || !is_digits(parts[2], 2)
    {
        return None;
    }
    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || year < 1 || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

/// Whether `part` is exactly `width` ASCII digits.
fn is_digits(part: &str, width: usize) -> bool {
    part.len() == width && part.bytes().all(|byte| byte.is_ascii_digit())
}

/// Days in `month` of `year` (Gregorian, leap years included).
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 31,
    }
}

/// `DOC-<unix millis>-<sequence>`: the sequence makes two validations in the
/// same millisecond distinct (round-27 macrorest-7 L3).
fn generate_document_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("DOC-{ts}-{seq}")
}

#[cfg(test)]
mod document_id_tests {
    use super::generate_document_id;

    #[test]
    fn same_millisecond_ids_are_distinct() {
        let ids: std::collections::HashSet<String> =
            (0..100).map(|_| generate_document_id()).collect();
        assert_eq!(ids.len(), 100);
        assert!(ids.iter().all(|id| id.starts_with("DOC-")));
    }
}

#[cfg(test)]
mod parse_date_tests {
    use super::parse_date;

    #[test]
    fn rejects_days_that_do_not_exist_in_the_month() {
        assert_eq!(parse_date("2025-02-31"), None);
        assert_eq!(parse_date("2025-04-31"), None);
        assert_eq!(parse_date("2025-02-29"), None);
        assert_eq!(parse_date("1900-02-29"), None);
        assert_eq!(parse_date("2025-01-00"), None);
    }

    /// The message promises `YYYY-MM-DD`: no sign, no missing zero padding,
    /// no stray characters.
    #[test]
    fn rejects_anything_but_the_zero_padded_format() {
        for text in [
            "2025-1-1",
            "2025-01-1",
            "25-01-01",
            "+025-01-01",
            "2025-+1-+1",
            "2025-01-01 ",
            " 2025-01-01",
            "2025/01/01",
        ] {
            assert_eq!(parse_date(text), None, "{text}");
        }
    }

    #[test]
    fn accepts_real_calendar_dates() {
        assert_eq!(parse_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_date("2000-02-29"), Some((2000, 2, 29)));
        assert_eq!(parse_date("2025-04-30"), Some((2025, 4, 30)));
        assert_eq!(parse_date("2025-12-31"), Some((2025, 12, 31)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_errors(title: String) -> Vec<String> {
        let request = ValidateDocumentRequest {
            document_type: "memo".into(),
            title,
            content: "one two three four five six seven eight nine ten".into(),
            author: String::new(),
            department: String::new(),
            classification: "public".into(),
            effective_date: "2026-01-01".into(),
            expiry_date: None,
        };
        validate_document(&request)
            .errors
            .into_iter()
            .filter(|error| error.code == "TITLE_TOO_LONG")
            .map(|error| error.message)
            .collect()
    }

    /// The limit counts characters, not UTF-8 bytes: 200 Korean characters
    /// (600 bytes) fit, 201 are reported with their character count.
    #[test]
    fn title_limit_counts_characters() {
        assert_eq!(
            title_errors("\u{AC00}".repeat(MAX_TITLE_LEN)),
            Vec::<String>::new()
        );
        assert_eq!(
            title_errors("\u{AC00}".repeat(MAX_TITLE_LEN + 1)),
            ["Title exceeds 200 characters (got 201)"]
        );
    }

    fn validation(document_type: &str, classification: &str, author: &str) -> ValidationResult {
        validate_document(&ValidateDocumentRequest {
            document_type: document_type.into(),
            title: "Title".into(),
            content: "one two three four five six seven eight nine ten".into(),
            author: author.into(),
            department: String::new(),
            classification: classification.into(),
            effective_date: "2026-01-01".into(),
            expiry_date: None,
        })
    }

    /// An unrecognised classification has no level and no retention, is not
    /// reported as a public regulation, and gets the author / department rules
    /// of a classified document (it used to count as level 0 throughout).
    #[test]
    fn unknown_classification_has_no_level_and_gets_the_classified_rules() {
        let result = validation("regulation", "top-secret-ish", "");
        let codes: Vec<&str> = result.errors.iter().map(|e| e.code.as_str()).collect();
        assert!(codes.contains(&"CLASSIFICATION_INVALID"), "{codes:?}");
        assert!(
            codes.contains(&"AUTHOR_REQUIRED_FOR_CLASSIFIED"),
            "{codes:?}"
        );
        assert!(codes.contains(&"DEPT_REQUIRED_FOR_CLASSIFIED"), "{codes:?}");
        assert!(
            result
                .warnings
                .iter()
                .all(|w| w.code != "REGULATION_PUBLIC")
        );
        assert_eq!(result.metadata.classification_level, None);
        assert_eq!(result.metadata.retention_years, None);
    }

    /// A public regulation is still warned about, and a known level carries
    /// its level and retention.
    #[test]
    fn known_classification_keeps_its_level_and_retention() {
        let public = validation("regulation", "public", "Kim");
        assert!(
            public
                .warnings
                .iter()
                .any(|w| w.code == "REGULATION_PUBLIC")
        );
        assert_eq!(public.metadata.classification_level, Some(0));
        assert_eq!(public.metadata.retention_years, Some(1));
        let secret = validation("memo", "secret", "Kim");
        assert!(
            secret
                .warnings
                .iter()
                .all(|w| w.code != "REGULATION_PUBLIC")
        );
        assert_eq!(secret.metadata.classification_level, Some(3));
        assert_eq!(secret.metadata.retention_years, Some(30));
    }
}
