//! Crate-wide response aliases. `vespera!` expands a handler's return-type
//! alias even when it is defined in another file, as long as exactly one
//! definition exists under `src/`.

use vespera::axum::Json;

use crate::routes::error::ErrorResponse2;

/// `Result` of a JSON body or an [`ErrorResponse2`].
pub type ApiResult<T> = Result<Json<T>, ErrorResponse2>;
