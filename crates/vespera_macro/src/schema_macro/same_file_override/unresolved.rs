//! The resolution check of a storage-fallback `explicit` adapter (R23-SM3-02):
//! a name only the DTO's own module resolves is refused, because the proxy
//! copies it into the call-site module.

/// Walks a DTO's field types for the first single-name type `unresolved`
/// accepts.
struct Types<'a> {
    unresolved: &'a dyn Fn(&str) -> bool,
    found: Option<String>,
}

impl<'ast> syn::visit::Visit<'ast> for Types<'_> {
    fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
        if self.found.is_none() && ty.qself.is_none() && ty.path.segments.len() == 1 {
            let name = ty.path.segments[0].ident.to_string();
            if (self.unresolved)(&name) {
                self.found = Some(name);
            }
        }
        syn::visit::visit_type_path(self, ty);
    }
}

/// The first single-name type or serde `default` function of a storage-fallback
/// DTO that the call-site module cannot resolve (and that is not a primitive or
/// prelude name). Multi-segment paths are not judged: a crate name
/// (`serde_json::Value`) is as good a root as a module of the DTO's file, and
/// the two cannot be told apart from the stored definition alone.
pub(super) fn unresolved_at_call_site(dto: &syn::ItemStruct) -> Option<String> {
    use crate::schema_macro::module_scope::{Resolved, resolve_name};
    let call_site = crate::schema_macro::call_site_module().unwrap_or_default();
    let unresolved = |name: &str| {
        !crate::schema_macro::type_utils::is_prelude_name(name)
            && matches!(
                resolve_name(&[name.to_string()], &call_site),
                Resolved::Unknown
            )
    };
    let mut types = Types {
        unresolved: &unresolved,
        found: None,
    };
    for field in &dto.fields {
        syn::visit::Visit::visit_type(&mut types, &field.ty);
    }
    types.found.or_else(|| {
        dto.fields.iter().find_map(|field| {
            let Some(Some(function)) = crate::parser::extract_default(&field.attrs) else {
                return None;
            };
            let path: syn::Path = syn::parse_str(&function).ok()?;
            let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
            let root = segments.first().cloned().unwrap_or_default();
            let relative = matches!(root.as_str(), "self" | "super")
                || (segments.len() == 1 && unresolved(&root));
            relative.then(|| segments.join("::"))
        })
    })
}
