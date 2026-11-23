//! Whether the items a module defines can be named from the call-site module
//! (the module a `schema_type!` DTO is emitted in). A private item is readable
//! from its module and that module's descendants, `pub(super)` / `pub(in path)`
//! from the module they name.

use crate::parser::strip_raw_prefix_owned;

use super::{ModuleScope, module_scope};

/// The module path an item of visibility `vis` declared in `source_module` can
/// be named from (its descendants included): `None` when it is `pub`.
pub(super) fn read_scope(vis: &syn::Visibility, source_module: &[String]) -> Option<Vec<String>> {
    let scope: Vec<String> = match vis {
        syn::Visibility::Public(_) => return None,
        syn::Visibility::Inherited => source_module.to_vec(),
        syn::Visibility::Restricted(restricted) => {
            let mut scope = source_module.to_vec();
            for segment in &restricted.path.segments {
                match strip_raw_prefix_owned(segment.ident.to_string()).as_str() {
                    "crate" => scope = vec!["crate".to_string()],
                    "self" => {}
                    "super" => {
                        scope.pop();
                    }
                    name => scope.push(name.to_string()),
                }
            }
            scope
        }
    };
    Some(scope)
}

/// Whether an item of visibility `vis` declared in `source_module` can be
/// named from `call_site` (`None`: unknown, as in unit tests — taken as
/// readable).
pub(in crate::schema_macro) fn readable_from(
    vis: &syn::Visibility,
    source_module: &[String],
    call_site: Option<&[String]>,
) -> bool {
    match (call_site, read_scope(vis, source_module)) {
        (Some(call_site), Some(scope)) => call_site.starts_with(&scope),
        _ => true,
    }
}

impl ModuleScope {
    /// Whether the item `name` this module defines is readable from
    /// `call_site`. Names it does not define (imports, prelude) are taken as
    /// readable: their own module decides.
    pub fn item_readable_from(&self, name: &str, call_site: Option<&[String]>) -> bool {
        match (call_site, self.read_scopes.get(name)) {
            (Some(call_site), Some(Some(scope))) => call_site.starts_with(scope),
            _ => true,
        }
    }
}

/// Whether every item along the crate-absolute `path` (`crate::models::memo::
/// Status`: each segment after the first, in the module before it) is
/// readable from `call_site`. A module that cannot be found is readable: the
/// compiler reports it.
pub fn path_readable_from(path: &[String], call_site: Option<&[String]>) -> bool {
    (1..path.len()).all(|end| {
        module_scope(&path[..end])
            .is_none_or(|scope| scope.item_readable_from(&path[end], call_site))
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::public("pub", "crate::routes", true)]
    #[case::crate_visible("pub(crate)", "crate::routes", true)]
    #[case::private_elsewhere("", "crate::routes", false)]
    #[case::private_same_module("", "crate::models::user", true)]
    #[case::private_child_module("", "crate::models::user::dto", true)]
    #[case::super_sibling("pub(super)", "crate::models::post", true)]
    #[case::super_outside("pub(super)", "crate::routes", false)]
    #[case::in_path("pub(in crate::models)", "crate::models::post", true)]
    #[case::in_path_outside("pub(in crate::models)", "crate::routes", false)]
    #[case::self_restricted("pub(self)", "crate::routes", false)]
    #[case::raw_segment("pub(in crate::r#models)", "crate::models", true)]
    fn readable_from_follows_visibility(
        #[case] vis: &str,
        #[case] call_site: &str,
        #[case] expected: bool,
    ) {
        let vis: syn::Visibility = syn::parse_str(vis).unwrap();
        let source = ["crate", "models", "user"].map(String::from);
        let call_site: Vec<String> = call_site.split("::").map(String::from).collect();
        assert_eq!(readable_from(&vis, &source, Some(&call_site)), expected);
        assert!(readable_from(&vis, &source, None), "unknown call site");
    }
}
