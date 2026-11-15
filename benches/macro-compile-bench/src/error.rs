//! A crate-wide handler result alias, declared outside the route files so
//! every `#[route]` returning it exercises the crate-wide alias lookup
//! (`src/` listing + scan for `type ApiResult`).

use vespera::axum::Json;

use crate::models::schemas::ErrorBody;

/// What most handlers of a real app return.
pub type ApiResult<T> = Result<Json<T>, Json<ErrorBody>>;
