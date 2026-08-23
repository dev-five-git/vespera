//! [`TryFromFieldWithState`] implementations for scalar multipart fields
//! (`String`, `bool`, the integer / float types, and `char`).
//!
//! Split out of `multipart.rs` to keep that file within the repository's
//! 1000-line source cap. `use super::*` keeps the shared helpers in scope —
//! [`read_field_data`], [`MeteredField`], `tiny_scalar_limit`, `str_to_bool`,
//! the capacity / limit constants, and [`TypedMultipartError`].

use std::borrow::Cow;

use super::{
    DEFAULT_STRING_FIELD_LIMIT_BYTES, MeteredField, STRING_INITIAL_CAPACITY_BYTES,
    TINY_SCALAR_INITIAL_CAPACITY_BYTES, TryFromFieldWithState, TypedMultipartError,
    read_field_data, str_to_bool, tiny_scalar_limit, truncate_reflected_value,
};

/// Single source of the [`TypedMultipartError::WrongFieldType`] value every
/// scalar parser in this module reports.
///
/// The seven call sites below differed only in `wanted` and `source`, so the
/// field-name extraction (`unwrap_or_default().to_owned()`) and the
/// `Cow::Borrowed` wrapping live here once. `source` is taken as
/// [`Display`](std::fmt::Display) rather than `String` so a caller can hand
/// over the borrowed parse error (or a `format_args!`) and let the single
/// `to_string()` below own the allocation.
fn wrong_field_type(
    field: &MeteredField<'_>,
    wanted: &'static str,
    source: impl std::fmt::Display,
) -> TypedMultipartError {
    TypedMultipartError::WrongFieldType {
        field_name: field.name().unwrap_or_default().to_owned(),
        wanted: Cow::Borrowed(wanted),
        source: source.to_string(),
    }
}

impl<S: Send + Sync> TryFromFieldWithState<S> for String {
    async fn try_from_field_with_state(
        field: MeteredField<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        // An ABSENT limit (`None`) applies the generous default cap; an
        // explicit `#[form_data(limit = "unlimited")]` arrives as
        // `Some(usize::MAX)` (set by the derive macro) and stays unbounded;
        // an explicit byte size wins as `Some(n)`.
        let limit = limit_bytes.unwrap_or(DEFAULT_STRING_FIELD_LIMIT_BYTES);
        let field_data = read_field_data(field, Some(limit), STRING_INITIAL_CAPACITY_BYTES).await?;
        let super::FieldBytes { field, data } = field_data;
        Self::from_utf8(data).map_err(|e| wrong_field_type(&field, "String", e))
    }
}

// ─── bool ───────────────────────────────────────────────────────────────────

impl<S: Send + Sync> TryFromFieldWithState<S> for bool {
    async fn try_from_field_with_state(
        field: MeteredField<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let field_data = read_field_data(
            field,
            Some(tiny_scalar_limit(limit_bytes)),
            TINY_SCALAR_INITIAL_CAPACITY_BYTES,
        )
        .await?;
        let super::FieldBytes { field, data } = field_data;
        let text = std::str::from_utf8(&data).map_err(|e| wrong_field_type(&field, "bool", e))?;
        str_to_bool(text).ok_or_else(|| {
            wrong_field_type(
                &field,
                "bool",
                format_args!(
                    "invalid boolean value: `{}`",
                    truncate_reflected_value(text)
                ),
            )
        })
    }
}

// ─── Numeric types ──────────────────────────────────────────────────────────

macro_rules! impl_try_from_field_for_number {
    ($($ty:ty),* $(,)?) => {
        $(
                impl<S: Send + Sync> TryFromFieldWithState<S> for $ty {
                async fn try_from_field_with_state(
                    field: MeteredField<'_>,
                    limit_bytes: Option<usize>,
                    _state: &S,
                ) -> Result<Self, TypedMultipartError> {
                    let field_data = read_field_data(
                        field,
                        Some(tiny_scalar_limit(limit_bytes)),
                        TINY_SCALAR_INITIAL_CAPACITY_BYTES,
                    ).await?;
                    let super::FieldBytes { field, data } = field_data;
                    let text = std::str::from_utf8(&data).map_err(|e| {
                        wrong_field_type(&field, stringify!($ty), e)
                    })?;
                    text.trim().parse::<$ty>().map_err(|e| {
                        wrong_field_type(&field, stringify!($ty), e)
                    })
                }
            }
        )*
    };
}

impl_try_from_field_for_number!(
    i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, isize, usize, f32, f64,
);

// ─── char ───────────────────────────────────────────────────────────────────

impl<S: Send + Sync> TryFromFieldWithState<S> for char {
    async fn try_from_field_with_state(
        field: MeteredField<'_>,
        limit_bytes: Option<usize>,
        _state: &S,
    ) -> Result<Self, TypedMultipartError> {
        let field_data = read_field_data(
            field,
            Some(tiny_scalar_limit(limit_bytes)),
            TINY_SCALAR_INITIAL_CAPACITY_BYTES,
        )
        .await?;
        let super::FieldBytes { field, data } = field_data;
        let text = std::str::from_utf8(&data).map_err(|e| wrong_field_type(&field, "char", e))?;
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(wrong_field_type(
                &field,
                "char",
                "expected exactly one character",
            )),
        }
    }
}
