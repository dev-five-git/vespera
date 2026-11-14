//! Validation of the route folder named by `dir = "..."` / `VESPERA_DIR`.
//!
//! The folder doubles as the module path of every route (`src/api/v1` →
//! `api::v1::users::list`), so it must be a path relative to `src/` whose
//! segments are module identifiers.  Anything else — an absolute path, `..`,
//! `\`, an empty segment, a keyword — used to fall through to a
//! working-directory lookup and generate unresolvable `crate::...` paths.

use proc_macro2::Span;

/// Check a route-folder name; `span` locates the error.
pub fn validate_route_dir(dir: &str, macro_label: &str, span: Span) -> syn::Result<()> {
    // A raw identifier (`r#type`) names the folder `type`, so `#` is out.
    let valid = !dir.is_empty()
        && !dir.contains('#')
        && dir
            .split('/')
            .all(|segment| syn::parse_str::<syn::Ident>(segment).is_ok());
    if valid {
        return Ok(());
    }
    Err(syn::Error::new(
        span,
        format!(
            "{macro_label} macro: invalid route folder `dir = \"{dir}\"`. It must be a folder \
             under `src/` named by module identifiers separated by `/`, e.g. `\"routes\"` or \
             `\"api/v1\"` (no absolute paths, `..`, `\\`, or empty segments)."
        ),
    ))
}

/// The Rust module path of a route folder (`"api/v1"` → `"api::v1"`).
pub fn route_dir_module_path(dir: &str) -> String {
    dir.replace('/', "::")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("routes")]
    #[case::nested("api/v1")]
    fn accepts_module_folders(#[case] dir: &str) {
        validate_route_dir(dir, "vespera!", Span::call_site()).unwrap();
    }

    #[test]
    fn rejects_other_folders() {
        let report: Vec<String> = [
            "",
            "/srv/routes",
            "../routes",
            "api\\v1",
            "routes/",
            "mod",
            "my-routes",
            "r#type",
        ]
        .iter()
        .map(|dir| {
            validate_route_dir(dir, "export_app!", Span::call_site())
                .unwrap_err()
                .to_string()
        })
        .collect();
        insta::assert_snapshot!("route_dir_rejections", report.join("\n"));
    }

    #[test]
    fn nested_folder_maps_to_a_module_path() {
        assert_eq!(route_dir_module_path("api/v1"), "api::v1");
        assert_eq!(route_dir_module_path("routes"), "routes");
    }
}
