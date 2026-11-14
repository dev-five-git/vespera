//! The `(method, path)` pairs one router claims, and the overlaps axum rejects
//! while building it.
//!
//! axum (matchit) panics on two kinds of overlap:
//!
//! * the same method on the same path — whatever the parameter names
//!   (`GET /users/{id}` twice);
//! * one path *shape* spelled with different parameter names at the same
//!   position — `/users/{id}` next to `/users/{user_id}` — **regardless of the
//!   method**: both strings are inserted into one matchit tree, which refuses
//!   the second spelling.
//!
//! [`RouteClaims`] detects both, so the overlap is a compile error instead of
//! a panic on the first `Router` build.

use std::collections::HashMap;

use crate::parser::openapi_path_template;

/// The shape of an axum path (`{name}` → `{}`, `{*name}` → `{*}`) and its
/// parameter names in order.  `{{` / `}}` escapes stay literal.
pub fn path_shape(path: &str) -> (String, Vec<String>) {
    let mut shape = String::with_capacity(path.len());
    let mut names = Vec::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.next_if_eq(&'{').is_some() => shape.push_str("{{"),
            '{' => {
                let catch_all = chars.next_if_eq(&'*').is_some();
                names.push(chars.by_ref().take_while(|&c| c != '}').collect());
                shape.push_str(if catch_all { "{*}" } else { "{}" });
            }
            c => shape.push(c),
        }
    }
    (shape, names)
}

/// Why [`RouteClaims::claim`] refused a route.
#[derive(Debug, PartialEq, Eq)]
pub enum RouteConflict {
    /// The same method and path are already served by `previous`.
    Duplicate {
        /// Upper-case HTTP method.
        method: String,
        /// Owner of the earlier claim.
        previous: String,
    },
    /// `previous_path` has the same shape but other parameter names.
    ParamNames {
        /// The earlier spelling of the shape.
        previous_path: String,
        /// Owner of the earlier spelling.
        previous: String,
    },
}

impl RouteConflict {
    /// The diagnostic for `owner` claiming `method path`.
    pub fn message(&self, path: &str, owner: &str) -> String {
        match self {
            Self::Duplicate { method, previous } => format!(
                "duplicate route: `{method} {path}` is defined by both `{previous}` and \
                 `{owner}` — each (method, path) pair must map to exactly one handler"
            ),
            Self::ParamNames {
                previous_path,
                previous,
            } => format!(
                "conflicting route parameters: `{path}` (`{owner}`) and `{previous_path}` \
                 (`{previous}`) are the same route with different parameter names — axum \
                 panics on them when the router is built; use the same parameter names"
            ),
        }
    }
}

/// Every path one router serves, keyed both exactly and by shape.
#[derive(Default)]
pub struct RouteClaims {
    /// `(METHOD, OpenAPI path template)` → owner.
    exact: HashMap<(String, String), String>,
    /// Path shape → (first spelling, its parameter names, owner).
    shapes: HashMap<String, (String, Vec<String>, String)>,
}

impl RouteClaims {
    /// Claim `method path` for `owner`; `path` is an axum path or an OpenAPI
    /// template (a merged child's spec only has the latter).
    pub fn claim(&mut self, method: &str, path: &str, owner: &str) -> Result<(), RouteConflict> {
        let method = method.to_ascii_uppercase();
        let (shape, names) = path_shape(path);
        if let Some((previous_path, previous_names, previous)) = self.shapes.get(&shape)
            && *previous_names != names
        {
            return Err(RouteConflict::ParamNames {
                previous_path: previous_path.clone(),
                previous: previous.clone(),
            });
        }
        let key = (method, openapi_path_template(path));
        if let Some(previous) = self.exact.get(&key) {
            return Err(RouteConflict::Duplicate {
                method: key.0,
                previous: previous.clone(),
            });
        }
        self.shapes
            .entry(shape)
            .or_insert_with(|| (path.to_owned(), names, owner.to_owned()));
        self.exact.insert(key, owner.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::static_path("/users", "/users", &[])]
    #[case::param("/users/{id}", "/users/{}", &["id"])]
    #[case::catch_all("/files/{*rest}", "/files/{*}", &["rest"])]
    #[case::partial_segment("/f/{name}.json", "/f/{}.json", &["name"])]
    #[case::escaped_braces("/a/{{x}}/{id}", "/a/{{x}}/{}", &["id"])]
    fn shapes_hide_parameter_names(
        #[case] path: &str,
        #[case] shape: &str,
        #[case] names: &[&str],
    ) {
        let (got_shape, got_names) = path_shape(path);
        assert_eq!(got_shape, shape);
        assert_eq!(got_names, names);
    }

    #[rstest]
    #[case::other_method_same_names(&[("get", "/u/{id}"), ("delete", "/u/{id}")], None)]
    #[case::static_next_to_param(&[("get", "/u/me"), ("get", "/u/{id}")], None)]
    #[case::param_vs_catch_all(&[("get", "/u/{id}"), ("post", "/u/{*rest}")], None)]
    #[case::same_method_same_path(
        &[("get", "/u/{id}"), ("GET", "/u/{id}")],
        Some("duplicate route: `GET /u/{id}` is defined by both `first` and `second` — each (method, path) pair must map to exactly one handler")
    )]
    #[case::param_names_differ_other_method(
        &[("get", "/u/{id}"), ("delete", "/u/{user_id}")],
        Some("conflicting route parameters: `/u/{user_id}` (`second`) and `/u/{id}` (`first`) are the same route with different parameter names — axum panics on them when the router is built; use the same parameter names")
    )]
    #[case::catch_all_names_differ(
        &[("get", "/f/{*a}"), ("get", "/f/{*b}")],
        Some("conflicting route parameters: `/f/{*b}` (`second`) and `/f/{*a}` (`first`) are the same route with different parameter names — axum panics on them when the router is built; use the same parameter names")
    )]
    fn claims_reject_what_axum_rejects(
        #[case] routes: &[(&str, &str)],
        #[case] expected: Option<&str>,
    ) {
        let mut claims = RouteClaims::default();
        let (method, path) = routes[0];
        claims.claim(method, path, "first").unwrap();
        let (method, path) = routes[1];
        let got = claims
            .claim(method, path, "second")
            .err()
            .map(|conflict| conflict.message(path, "second"));
        assert_eq!(got.as_deref(), expected);
    }
}
