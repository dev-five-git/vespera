//! The resolution check of a storage-fallback `explicit` adapter (R23-SM3-02,
//! R24-SM3-01/02): a name the DTO's own module resolves differently from the
//! call site (or not at all) is refused, because the proxy copies it into the
//! call-site module.

use syn::parse::Parser;
use syn::punctuated::Punctuated;

/// Whether a name the copied text uses is one the call site cannot resolve.
type Judge<'a> = &'a dyn Fn(&str) -> bool;

/// Walks a DTO's field types for the first single-name type `unresolved`
/// accepts.
struct Types<'a> {
    unresolved: Judge<'a>,
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

/// The `Meta` items of a comma-separated list (`tokens` of a `serde(..)`).
fn metas(tokens: proc_macro2::TokenStream) -> Vec<syn::Meta> {
    Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
        .parse2(tokens)
        .map(|items| items.into_iter().collect())
        .unwrap_or_default()
}

/// The `serde(..)` items of `attrs`: a plain `serde(..)` and the ones a
/// `cfg_attr(predicate, serde(..))` adds (the predicate is not one).
fn serde_metas(attrs: &[syn::Attribute]) -> Vec<syn::Meta> {
    attrs
        .iter()
        .flat_map(|attr| {
            if attr.path().is_ident("serde") {
                attr.meta
                    .require_list()
                    .map(|list| metas(list.tokens.clone()))
                    .unwrap_or_default()
            } else if attr.path().is_ident("cfg_attr") {
                attr.meta
                    .require_list()
                    .map(|list| metas(list.tokens.clone()))
                    .unwrap_or_default()
                    .into_iter()
                    .skip(1)
                    .filter_map(|meta| match meta {
                        syn::Meta::List(list) if list.path.is_ident("serde") => {
                            Some(metas(list.tokens))
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// The `key = "value"` string items among `metas`.
fn string_items(metas: &[syn::Meta]) -> Vec<(String, String)> {
    metas
        .iter()
        .filter_map(|meta| match meta {
            syn::Meta::NameValue(nv) => match (nv.path.get_ident(), &nv.value) {
                (
                    Some(key),
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit),
                        ..
                    }),
                ) => Some((key.to_string(), lit.value())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A serde function / module path copied into the proxy (`default = "f"`,
/// `with = "codec::parse"`) whose root the call site cannot resolve. A root
/// naming a crate the compiler links (`serde_json::Value`) is no module of
/// the DTO's file, so it is not judged.
fn path_is_unresolved(text: &str, unresolved: Judge) -> Option<String> {
    let path: syn::Path = syn::parse_str(text).ok()?;
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    let root = segments.first()?;
    let relative = matches!(root.as_str(), "self" | "super")
        || (!crate::schema_macro::file_lookup::is_extern_crate(root) && unresolved(root));
    relative.then(|| segments.join("::"))
}

/// A serde type copied into the proxy (`from = "Wire"`, `try_from = ..`): its
/// first single-name type the call site cannot resolve.
fn type_is_unresolved(text: &str, unresolved: Judge) -> Option<String> {
    let ty: syn::Type = syn::parse_str(text).ok()?;
    let mut types = Types {
        unresolved,
        found: None,
    };
    syn::visit::Visit::visit_type(&mut types, &ty);
    types.found
}

/// The first single-name type or serde path of a storage-fallback DTO that the
/// call site cannot resolve the way the DTO's module does (and that is not a
/// primitive or prelude name). The check covers the field types and every
/// serde function / type path the proxy copies: field `default`,
/// `skip_serializing_if`, `with`, `serialize_with`, `deserialize_with`, and
/// container `default`, `from`, `try_from` (R24-SM3-01). `dto_module` is the
/// DTO's own module (`None`: unknown, so only unresolved names are judged); a
/// name that resolves to another item there than at the call site is refused
/// too, since the proxy would name the call-site item (R24-SM3-02).
/// Multi-segment type paths are not judged (their roots cannot be told apart
/// from the stored definition alone).
pub(super) fn unresolved_at_call_site(
    dto: &syn::ItemStruct,
    dto_module: Option<&[String]>,
) -> Option<String> {
    use crate::schema_macro::module_scope::{Resolved, resolve_name};
    let call_site = crate::schema_macro::call_site_module().unwrap_or_default();
    let unresolved = |name: &str| {
        if crate::schema_macro::type_utils::is_prelude_name(name) {
            return false;
        }
        match resolve_name(&[name.to_string()], &call_site) {
            Resolved::Unknown => true,
            Resolved::Path(here) => dto_module.is_some_and(|module| {
                matches!(
                    resolve_name(&[name.to_string()], module),
                    Resolved::Path(there) if there != here
                )
            }),
        }
    };
    let mut types = Types {
        unresolved: &unresolved,
        found: None,
    };
    for field in &dto.fields {
        syn::visit::Visit::visit_type(&mut types, &field.ty);
    }
    let serde_check = |attrs: &[syn::Attribute]| {
        string_items(&serde_metas(attrs))
            .into_iter()
            .find_map(|(key, value)| match key.as_str() {
                "from" | "try_from" => type_is_unresolved(&value, &unresolved),
                "default"
                | "skip_serializing_if"
                | "with"
                | "serialize_with"
                | "deserialize_with" => path_is_unresolved(&value, &unresolved),
                _ => None,
            })
    };
    types.found.or_else(|| serde_check(&dto.attrs)).or_else(|| {
        dto.fields
            .iter()
            .find_map(|field| serde_check(&field.attrs))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_macro::file_cache::bump_epoch;
    use crate::schema_macro::file_lookup::set_test_call_site_module;

    /// R24-SM3-01: every serde path and type the proxy copies is judged, not
    /// only a field's `default`.
    #[rstest::rstest]
    #[case::deserialize_with(
        "struct D { #[serde(deserialize_with = \"codec::parse_id\")] id: i32 }",
        Some("codec::parse_id")
    )]
    #[case::skip_serializing_if(
        "struct D { #[serde(skip_serializing_if = \"is_zero\")] id: i32 }",
        Some("is_zero")
    )]
    #[case::cfg_attr_serialize_with(
        "struct D { #[cfg_attr(feature = \"x\", serde(serialize_with = \"codec::write\"))] id: i32 }",
        Some("codec::write")
    )]
    #[case::container_default("#[serde(default = \"make\")] struct D { id: i32 }", Some("make"))]
    #[case::container_from("#[serde(from = \"Wire\")] struct D { id: i32 }", Some("Wire"))]
    // A `Self::` root is a prelude-like name the call site always has.
    #[case::self_root("struct D { #[serde(with = \"Self::codec\")] id: i32 }", None)]
    // A multi-segment type is not judged (its root is not told apart).
    #[case::multi_segment_from("#[serde(try_from = \"codec::Wire\")] struct D { id: i32 }", None)]
    // A serde value that is no string literal is no copied path.
    #[case::non_string_value("struct D { #[serde(with = codec)] id: i32 }", None)]
    fn copied_serde_paths_are_judged(#[case] source: &str, #[case] expected: Option<&str>) {
        let _call = set_test_call_site_module("crate::b");
        let dto: syn::ItemStruct = syn::parse_str(source).unwrap();
        assert_eq!(
            unresolved_at_call_site(&dto, None),
            expected.map(str::to_string)
        );
    }

    /// R24-SM3-02: a name both modules resolve, to DIFFERENT items, is refused;
    /// the same item on both sides is not.
    #[test]
    fn name_resolving_to_another_item_at_the_call_site_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (file, text) in [
            ("lib.rs", "pub mod a;\npub mod b;\npub mod x;\npub mod y;"),
            ("a.rs", "use crate::x::Kind;"),
            ("b.rs", "use crate::y::Kind;"),
            ("x.rs", "pub struct Kind;"),
            ("y.rs", "pub struct Kind;"),
        ] {
            std::fs::write(src.join(file), text).unwrap();
        }
        let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        let _call = set_test_call_site_module("crate::b");
        let dto: syn::ItemStruct = syn::parse_str("struct D { kind: Kind }").unwrap();
        let dto_in = |module: &[&str]| module.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            unresolved_at_call_site(&dto, Some(&dto_in(&["crate", "a"]))),
            Some("Kind".to_string())
        );
        assert_eq!(
            unresolved_at_call_site(&dto, Some(&dto_in(&["crate", "b"]))),
            None
        );
    }
}
