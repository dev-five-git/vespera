//! The `VESPERA_*` environment fallbacks of `vespera!` / `export_app!` inputs.

use proc_macro2::Span;
use syn::LitStr;

/// The `VESPERA_*` variable `key` as a string literal, when set and not empty.
/// A non-Unicode value is an error naming the variable.
pub(super) fn env_lit(key: &str) -> syn::Result<Option<LitStr>> {
    Ok(crate::env_access::non_empty_var(key)?.map(|value| LitStr::new(&value, Span::call_site())))
}

/// `arg`, else the `VESPERA_*` variable `key` (the variable is read only when
/// `arg` is absent).
pub(super) fn arg_or_env(arg: Option<LitStr>, key: &str) -> syn::Result<Option<LitStr>> {
    arg.map_or_else(|| env_lit(key), |arg| Ok(Some(arg)))
}

/// A documentation URL argument, else its env-var fallback, with the label
/// errors name it by: the argument, or the variable it was read from (the
/// error points at the whole macro call, so the argument name alone sent
/// readers looking for an argument they never wrote). An empty variable (a
/// `VESPERA_DOCS_URL=` export) counts as unset, so it cannot fail the build
/// with "must start with `/`".
pub(super) fn doc_url_or_env(
    arg: Option<LitStr>,
    key: &str,
    var: &str,
) -> syn::Result<(Option<LitStr>, String)> {
    match arg {
        Some(lit) => Ok((Some(lit), format!("`{key}`"))),
        None => Ok((
            env_lit(var)?,
            format!("`{key}` (from the `{var}` environment variable)"),
        )),
    }
}
