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
//!   the second spelling;
//! * a parameter and a catch-all at the same position — `/files/{name}` next
//!   to `/files/{*path}`, `/{id}` next to `/{*rest}`, `/cmd/{tool}/{sub}` next
//!   to `/cmd/{*path}` — again regardless of the method;
//! * anything after a catch-all (`/files/{*path}/raw`).
//!
//! [`RouteClaims`] detects all of them, so the overlap is a compile error instead of
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

/// The kind of wildcard at one position of a path shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WildcardKind {
    /// `{name}`.
    Param,
    /// `{*name}`.
    CatchAll,
    /// A path-final `{name}` of a merged child's OpenAPI template: the
    /// template dropped the `*`, so it may have been either.
    Unknown,
}

/// Every wildcard of `shape` (see [`path_shape`]) as (static shape prefix
/// before it, kind); `None` when something follows a catch-all.
fn wildcard_positions(shape: &str, template: bool) -> Option<Vec<(String, WildcardKind)>> {
    let mut positions = Vec::new();
    let mut prefix = String::with_capacity(shape.len());
    let mut chars = shape.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            prefix.push(c);
            continue;
        }
        if chars.next_if_eq(&'{').is_some() {
            prefix.push_str("{{");
            continue;
        }
        let catch_all = chars.next_if_eq(&'*').is_some();
        chars.next_if_eq(&'}');
        let last = chars.peek().is_none();
        let kind = if catch_all {
            if !last {
                return None;
            }
            WildcardKind::CatchAll
        } else if template && last {
            WildcardKind::Unknown
        } else {
            WildcardKind::Param
        };
        positions.push((prefix.clone(), kind));
        prefix.push_str(if catch_all { "{*}" } else { "{}" });
    }
    Some(positions)
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
    /// `previous_path` has a parameter where this path has a catch-all (or
    /// the other way round) at the same position.
    WildcardKind {
        /// The earlier path.
        previous_path: String,
        /// Owner of the earlier path.
        previous: String,
    },
    /// The path continues after a catch-all.
    CatchAllNotLast,
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
            Self::WildcardKind {
                previous_path,
                previous,
            } => format!(
                "conflicting route wildcards: `{path}` (`{owner}`) and `{previous_path}` \
                 (`{previous}`) put a parameter and a catch-all at the same position — axum \
                 panics on them when the router is built; use one kind of wildcard there"
            ),
            Self::CatchAllNotLast => format!(
                "invalid route: `{path}` (`{owner}`) continues after a catch-all — a \
                 `{{*name}}` parameter must end the path; axum panics on it when the router \
                 is built"
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
    /// Shape prefix before a wildcard → (its kind, first path, owner).
    wildcards: HashMap<String, (WildcardKind, String, String)>,
}

impl RouteClaims {
    /// Claim `method path` for `owner`; `path` is an axum path (catch-alls
    /// spelled `{*name}`).
    pub fn claim(&mut self, method: &str, path: &str, owner: &str) -> Result<(), RouteConflict> {
        self.claim_inner(method, path, owner, false)
    }

    /// Claim an OpenAPI template from a merged child's spec: the template
    /// dropped the `*` of a catch-all, so a path-final parameter may be
    /// either kind and never makes a wildcard-kind conflict.
    pub fn claim_template(
        &mut self,
        method: &str,
        path: &str,
        owner: &str,
    ) -> Result<(), RouteConflict> {
        self.claim_inner(method, path, owner, true)
    }

    fn claim_inner(
        &mut self,
        method: &str,
        path: &str,
        owner: &str,
        template: bool,
    ) -> Result<(), RouteConflict> {
        let method = method.to_ascii_uppercase();
        let (shape, names) = path_shape(path);
        let positions =
            wildcard_positions(&shape, template).ok_or(RouteConflict::CatchAllNotLast)?;
        if let Some((previous_path, previous_names, previous)) = self.shapes.get(&shape)
            && *previous_names != names
        {
            return Err(RouteConflict::ParamNames {
                previous_path: previous_path.clone(),
                previous: previous.clone(),
            });
        }
        for (prefix, kind) in &positions {
            if let Some((previous_kind, previous_path, previous)) = self.wildcards.get(prefix)
                && ![*kind, *previous_kind].contains(&WildcardKind::Unknown)
                && previous_kind != kind
            {
                return Err(RouteConflict::WildcardKind {
                    previous_path: previous_path.clone(),
                    previous: previous.clone(),
                });
            }
        }
        let key = (method, openapi_path_template(path));
        if let Some(previous) = self.exact.get(&key) {
            return Err(RouteConflict::Duplicate {
                method: key.0,
                previous: previous.clone(),
            });
        }
        for (prefix, kind) in positions {
            let entry = self
                .wildcards
                .entry(prefix)
                .or_insert_with(|| (kind, path.to_owned(), owner.to_owned()));
            if entry.0 == WildcardKind::Unknown {
                *entry = (kind, path.to_owned(), owner.to_owned());
            }
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
    #[case::param_vs_catch_all(
        &[("get", "/u/{id}"), ("post", "/u/{*rest}")],
        Some("conflicting route wildcards: `/u/{*rest}` (`second`) and `/u/{id}` (`first`) put a parameter and a catch-all at the same position — axum panics on them when the router is built; use one kind of wildcard there")
    )]
    #[case::catch_all_vs_param_same_method(
        &[("get", "/src/{*filepath}"), ("get", "/src/{file}")],
        Some("conflicting route wildcards: `/src/{file}` (`second`) and `/src/{*filepath}` (`first`) put a parameter and a catch-all at the same position — axum panics on them when the router is built; use one kind of wildcard there")
    )]
    #[case::deeper_param_vs_catch_all(
        &[("get", "/cmd/{tool}/{sub}"), ("get", "/cmd/{*path}")],
        Some("conflicting route wildcards: `/cmd/{*path}` (`second`) and `/cmd/{tool}/{sub}` (`first`) put a parameter and a catch-all at the same position — axum panics on them when the router is built; use one kind of wildcard there")
    )]
    #[case::root_param_vs_catch_all(
        &[("get", "/{id}"), ("delete", "/{*filepath}")],
        Some("conflicting route wildcards: `/{*filepath}` (`second`) and `/{id}` (`first`) put a parameter and a catch-all at the same position — axum panics on them when the router is built; use one kind of wildcard there")
    )]
    #[case::param_vs_catch_all_other_prefix(&[("get", "/a/{id}"), ("get", "/b/{*rest}")], None)]
    #[case::static_next_to_catch_all(&[("get", "/src/x{id}"), ("get", "/src/{*rest}")], None)]
    #[case::escaped_brace_is_static(&[("get", "/a/{{x}}"), ("get", "/a/{*rest}")], None)]
    #[case::text_after_catch_all(
        &[("get", "/u"), ("get", "/f/{*rest}/raw")],
        Some("invalid route: `/f/{*rest}/raw` (`second`) continues after a catch-all — a `{*name}` parameter must end the path; axum panics on it when the router is built")
    )]
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

#[cfg(test)]
mod template_tests {
    use rstest::rstest;

    use super::*;

    /// A merged child's template drops the `*`, so its path-final parameter is
    /// compatible with either kind; a non-final one is still a parameter.
    #[rstest]
    #[case::final_template_param_vs_catch_all("/f/{*rest}", "/f/{rest}", true)]
    #[case::final_template_param_vs_param("/f/{id}", "/f/{id}", true)]
    #[case::template_first_then_catch_all("/f/{rest}", "/f/{*rest}", true)]
    #[case::non_final_template_param_vs_catch_all("/f/{*rest}", "/f/{id}/x", false)]
    fn template_wildcards(#[case] first: &str, #[case] second: &str, #[case] compatible: bool) {
        let mut claims = RouteClaims::default();
        if first.contains('*') {
            claims.claim("get", first, "first").unwrap();
            let result = claims.claim_template("post", second, "second");
            assert_eq!(result.is_ok(), compatible, "{result:?}");
        } else {
            claims.claim_template("get", first, "first").unwrap();
            assert_eq!(claims.claim("post", second, "second").is_ok(), compatible);
            // The real kind replaced the unknown one: a parameter now conflicts.
            if second.contains('*') {
                assert!(matches!(
                    claims.claim("put", "/f/{rest}", "third"),
                    Err(RouteConflict::WildcardKind { .. })
                ));
            }
        }
    }
}
