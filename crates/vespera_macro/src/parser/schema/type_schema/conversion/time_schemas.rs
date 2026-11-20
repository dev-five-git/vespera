//! Leaf schemas whose serde wire shape depends on how the type is spelled:
//! the `time` crate's date / time values, `rust_decimal::Decimal` and
//! `Duration`. Sidecar of `conversion`.

use syn::ext::IdentExt;
use vespera_core::schema::{AdditionalProperties, Schema, SchemaRef, SchemaType};

use crate::parser::schema::request_view;

use super::string_with_format;

/// Description of a `time` crate value (see [`time_crate_schema`]).
pub(super) const TIME_CRATE_DESCRIPTION: &str = "Serialized by the `time` crate: a string in \
its own format (not RFC 3339) with its `serde-human-readable` feature, an array of \
numbers otherwise.";

/// Description of a bare `Duration` (see [`duration_schema`]).
const AMBIGUOUS_DURATION_DESCRIPTION: &str = "A `Duration` whose crate is not \
spelled out: `std::time::Duration` is `{\"secs\", \"nanos\"}`, `time::Duration` a \
string or an array. Write the full path or add `#[schema(value_type = T)]`.";

/// A `time` crate date / time value: a plain string (no RFC 3339 `format`).
pub(super) fn time_crate_schema() -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        description: Some(TIME_CRATE_DESCRIPTION.to_string()),
        ..Schema::string()
    }))
}

/// `time::<..>::Ident` — a path rooted at the `time` crate.
pub(super) fn is_time_crate_path(path: &syn::Path) -> bool {
    path.segments.len() > 1 && path.segments[0].ident.unraw() == "time"
}

/// `rust_decimal::Decimal`: a `decimal` string; in the request view also a
/// JSON number (see the `"Decimal"` arm of the primitive match).
pub(super) fn decimal_schema() -> SchemaRef {
    let string = string_with_format("decimal");
    if !request_view::in_request_view() {
        return string;
    }
    SchemaRef::Inline(Box::new(Schema {
        any_of: Some(vec![string, SchemaRef::Inline(Box::new(Schema::number()))]),
        ..Schema::default()
    }))
}

/// `std::time::Duration` / `core::time::Duration`.
pub fn is_std_duration_path(path: &syn::Path) -> bool {
    let segments = &path.segments;
    segments.len() == 3
        && (segments[0].ident == "std" || segments[0].ident == "core")
        && segments[1].ident == "time"
        && segments[2].ident == "Duration"
}

/// Schema of a `Duration`, chosen from how its path is spelled.
///
/// - `std::time::Duration` / `core::time::Duration`: serde's std impl writes
///   `{"secs": u64, "nanos": u32}` (nanos < 1e9) and rejects unknown keys;
///   it also READS a `[secs, nanos]` array, and any `nanos` (it normalises
///   them), so the request view is `anyOf: [<object>, <array>]`.
/// - a bare `Duration` may be std's or `time`'s (`use time::Duration`), which
///   serialize differently, so it is any value (`{}`) with a description.
/// - any other qualified `Duration` (`time::Duration`, `chrono::Duration`, …)
///   has no single wire shape (the `time` crate emits a `"secs.nanos"` string
///   or a `[secs, nanos]` tuple depending on its features; chrono needs a
///   `serde_with` adapter), so it is documented as any value (`{}`). Annotate
///   the field with `#[schema(...)]` / a newtype to document the exact shape.
pub(super) fn duration_schema(path: &syn::Path) -> SchemaRef {
    if path.segments.len() == 1 {
        return SchemaRef::Inline(Box::new(Schema {
            description: Some(AMBIGUOUS_DURATION_DESCRIPTION.to_string()),
            ..Schema::default()
        }));
    }
    if !is_std_duration_path(path) {
        return SchemaRef::Inline(Box::default());
    }
    let request = request_view::in_request_view();
    let secs = || {
        SchemaRef::Inline(Box::new(Schema {
            format: Some("uint64".to_string()),
            minimum: Some(0.0),
            ..Schema::integer()
        }))
    };
    let nanos = || {
        SchemaRef::Inline(Box::new(Schema {
            format: Some("uint32".to_string()),
            minimum: Some(0.0),
            maximum: Some(if request {
                f64::from(u32::MAX)
            } else {
                999_999_999.0
            }),
            ..Schema::integer()
        }))
    };
    let object = Schema {
        properties: Some(std::collections::BTreeMap::from([
            ("secs".to_string(), secs()),
            ("nanos".to_string(), nanos()),
        ])),
        required: Some(vec!["secs".to_string(), "nanos".to_string()]),
        additional_properties: Some(AdditionalProperties::Bool(false)),
        ..Schema::object()
    };
    if !request {
        return SchemaRef::Inline(Box::new(object));
    }
    SchemaRef::Inline(Box::new(Schema {
        any_of: Some(vec![
            SchemaRef::Inline(Box::new(object)),
            SchemaRef::Inline(Box::new(Schema {
                prefix_items: Some(vec![secs(), nanos()]),
                min_items: Some(2),
                max_items: Some(2),
                ..Schema::new(SchemaType::Array)
            })),
        ]),
        ..Schema::default()
    }))
}
