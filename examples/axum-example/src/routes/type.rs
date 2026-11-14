//! A keyword module (`mod r#type;`) with a raw-identifier handler.

/// Mounted as `crate::routes::r#type::r#match`; operationId `match`.
#[vespera::route(get)]
pub async fn r#match() -> &'static str {
    "match"
}
