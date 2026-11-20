//! The resolution check of a storage-fallback `explicit` adapter (R23-SM3-02,
//! R24-SM3-01/02): a name the DTO's own module resolves differently from the
//! call site (or not at all) is refused, because the proxy copies it into the
//! call-site module.

use syn::parse::Parser;
use syn::punctuated::Punctuated;

use crate::schema_macro::generate_type::assignments::readable_from;

/// Whether a name the copied text uses is one the call site cannot resolve.
type Judge<'a> = &'a dyn Fn(&str) -> bool;

/// Walks a DTO's field types for the first single-name type `unresolved`
/// accepts.
struct Types<'a> {
    unresolved: Judge<'a>,
    found: Option<String>,
}

impl Types<'_> {
    /// Judges the root of `path`: a single name is the item itself, a
    /// multi-segment one (`enums::Status`) names a module the call site must
    /// resolve too (R25-SM3-02). A crate the compiler links is no module of
    /// the DTO's file, so its root is not judged.
    fn judge(&mut self, path: &syn::Path) {
        if self.found.is_none()
            && let Some(root) = path.segments.first()
        {
            let name = root.ident.to_string();
            let judged = path.segments.len() == 1
                || !crate::schema_macro::file_lookup::is_extern_crate(&name);
            if judged && (self.unresolved)(&name) {
                self.found = Some(name);
            }
        }
    }
}

impl<'ast> syn::visit::Visit<'ast> for Types<'_> {
    fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
        if ty.qself.is_none() {
            self.judge(&ty.path);
        }
        syn::visit::visit_type_path(self, ty);
    }

    /// A const expression (an array length `[u8; HASH_LEN]`, a const generic
    /// argument `Digest<{ HASH_LEN }>`) names a const the call site must resolve
    /// too, judged by its root like a type path (R34-SM4-L1).
    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        if expr.qself.is_none() {
            self.judge(&expr.path);
        }
        syn::visit::visit_expr_path(self, expr);
    }
}

/// A root or prelude name every module resolves the way the std library does,
/// with no import at the call site (R25-SM3-01): the std crates and the
/// `std` prelude's traits and functions a serde path names (`Default::default`).
fn is_std_resolved(name: &str) -> bool {
    matches!(
        name,
        "std"
            | "core"
            | "alloc"
            | "Default"
            | "Clone"
            | "Copy"
            | "Drop"
            | "From"
            | "Into"
            | "TryFrom"
            | "TryInto"
            | "AsRef"
            | "AsMut"
            | "Iterator"
            | "IntoIterator"
            | "Extend"
            | "FromIterator"
            | "ToOwned"
            | "ToString"
            | "PartialEq"
            | "PartialOrd"
            | "Eq"
            | "Ord"
            | "Send"
            | "Sync"
            | "Sized"
            | "Unpin"
            | "Fn"
            | "FnMut"
            | "FnOnce"
            | "drop"
            | "String"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
    )
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
/// A multi-segment type path is judged by its root, as a serde path is
/// (R25-SM3-02); a root the compiler links (an `--extern` crate) and the std
/// crates and prelude names are never judged (R25-SM3-01).
pub(super) fn unresolved_at_call_site(
    dto: &syn::ItemStruct,
    dto_module: Option<&[String]>,
) -> Option<String> {
    use crate::schema_macro::module_scope::{Resolved, resolve_name};
    let call_site = crate::schema_macro::call_site_module().unwrap_or_default();
    let unresolved = |name: &str| {
        if crate::schema_macro::type_utils::is_prelude_name(name) || is_std_resolved(name) {
            return false;
        }
        match resolve_name(&[name.to_string()], &call_site) {
            Resolved::Unknown => true,
            // The DTO module must resolve the name to the same item. `Unknown`
            // there (a non-crate glob such as `sea_orm::prelude::*`) is a
            // difference too: the proxy would name the call-site item (R28-SM3-01).
            Resolved::Path(here) => dto_module.is_some_and(|module| {
                !matches!(
                    resolve_name(&[name.to_string()], module),
                    Resolved::Path(there) if there == here
                )
            }),
        }
    };
    let mut types = Types {
        unresolved: &unresolved,
        found: None,
    };
    let live_fields = || dto.fields.iter().filter(|field| !compiled_out(field));
    for field in live_fields() {
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
    types
        .found
        .or_else(|| serde_check(&dto.attrs))
        .or_else(|| live_fields().find_map(|field| serde_check(&field.attrs)))
}

/// Whether the compiler drops `field` under its statically false `cfg`s
/// (`cfg_attr(p, cfg(q))` included): the expansion never names it, so its
/// types, serde paths and readability are not the call site's concern
/// (R30-SM3-L4).
fn compiled_out(field: &syn::Field) -> bool {
    crate::schema_macro::cfg_eval::compiled_out(&field.attrs)
}

/// `pub` or `pub(crate)`: readable from every module of the crate, so the one
/// visibility known readable without the DTO's module (R30-SM3-L3).
fn public_or_crate(vis: &syn::Visibility) -> bool {
    match vis {
        syn::Visibility::Public(_) => true,
        syn::Visibility::Restricted(restricted) => restricted.path.is_ident("crate"),
        syn::Visibility::Inherited => false,
    }
}

/// Whether the wrapper's bare adapter name `adapter` resolves at the call site
/// to some other item than the stored one at `stored` (its defining module,
/// followed through re-exports). The wrapper writes the adapter by that bare
/// name, so a different same-named struct there would be the one used
/// (R30-SM3-L2, the R24-SM3-02 rule). A name the call site does not pin
/// (unknown, or only glob-imported) is left to rustc.
fn adapter_names_other_item(adapter: &str, stored: &[String]) -> bool {
    use crate::schema_macro::module_scope::{Resolved, resolve_name};
    let call_site = crate::schema_macro::call_site_module().unwrap_or_default();
    let Resolved::Path(mut path) = resolve_name(&[adapter.to_string()], &call_site) else {
        return false;
    };
    let defined = path.pop().unwrap_or_default();
    let (module, name) =
        crate::schema_macro::reexported_item(&path, &defined).unwrap_or((path, defined));
    module != stored || name != adapter
}

/// Refuses a storage-fallback adapter DTO the call site cannot use: a name it
/// does not resolve the way its own module does ([`unresolved_at_call_site`]),
/// or a field the call site cannot read (R29-SM3-L2). The wrapper builds the
/// DTO with a struct literal there, which names every field, so a private one
/// is E0451 inside the generated code; the error is spanned at the adapter.
pub(super) fn check_storage_fallback(
    field_name: &str,
    adapter_name: &syn::Ident,
    dto_name: &str,
    dto: &syn::ItemStruct,
    dto_module: Option<&[String]>,
) -> syn::Result<()> {
    if dto_module.is_some_and(|stored| adapter_names_other_item(dto_name, stored)) {
        return Err(syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` adapter `{dto_name}` is declared in another module, but `{dto_name}` at the call site names another item; import the declared `{dto_name}` here, or declare the adapter in this file"
            ),
        ));
    }
    if let Some(name) = unresolved_at_call_site(dto, dto_module) {
        return Err(syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` adapter `{dto_name}` is declared in another module and names `{name}`, which the call-site module does not resolve; import `{name}` here, use an absolute path in that struct, or declare the adapter in this file"
            ),
        ));
    }
    let call_site = crate::schema_macro::call_site_module();
    // Without the DTO's module nothing but `pub` / `pub(crate)` is known to be
    // readable: an inherited field must not pass as readable everywhere
    // (R30-SM3-L3).
    let readable = |field: &syn::Field| {
        dto_module.map_or_else(
            || public_or_crate(&field.vis),
            |module| readable_from(&field.vis, module, call_site.as_deref()),
        )
    };
    let private = dto
        .fields
        .iter()
        .filter(|field| !compiled_out(field))
        .find(|field| !readable(field))
        .and_then(|field| field.ident.as_ref());
    if let Some(ident) = private {
        return Err(syn::Error::new_spanned(
            adapter_name,
            format!(
                "schema_type!: relation field `{field_name}` adapter `{dto_name}` is declared in another module and its field `{ident}` is not readable from this module (the generated struct literal sets every field); make `{ident}` `pub` or declare the adapter in this file"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_macro::file_cache::bump_epoch;
    use crate::schema_macro::file_lookup::set_test_call_site_module;

    /// R29-SM3-L2: a private field of a storage-fallback DTO is refused, naming
    /// the field; `pub` / `pub(crate)` fields and a call site inside the DTO's
    /// module pass.
    #[rstest::rstest]
    #[case::private_field("struct D { id: i32 }", "crate::routes", Some("id"))]
    #[case::second_private_field(
        "struct D { pub id: i32, name: String }",
        "crate::routes",
        Some("name")
    )]
    #[case::public_field("struct D { pub id: i32 }", "crate::routes", None)]
    #[case::crate_visible_field("struct D { pub(crate) id: i32 }", "crate::routes", None)]
    #[case::same_module("struct D { id: i32 }", "crate::models::card", None)]
    // R30-SM3-L4: a field compiled out at the call site is never named.
    #[case::cfg_off_private_field(
        "struct D { pub id: i32, #[cfg(false)] name: String }",
        "crate::routes",
        None
    )]
    fn storage_fallback_fields_must_be_readable_at_the_call_site(
        #[case] src: &str,
        #[case] call_site: &str,
        #[case] refused: Option<&str>,
    ) {
        let _call = set_test_call_site_module(call_site);
        let dto: syn::ItemStruct = syn::parse_str(src).unwrap();
        let module = ["crate", "models", "card"].map(String::from);
        let adapter = syn::Ident::new("OwnerCard", proc_macro2::Span::call_site());
        let result = check_storage_fallback("owner", &adapter, "OwnerCard", &dto, Some(&module));
        match refused {
            Some(field) => {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(&format!("field `{field}`")), "{error}");
            }
            None => assert!(result.is_ok()),
        }
    }

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
    // R25-SM3-02: a multi-segment type is judged by its root.
    #[case::multi_segment_from(
        "#[serde(try_from = \"codec::Wire\")] struct D { id: i32 }",
        Some("codec")
    )]
    #[case::multi_segment_field_type("struct D { status: enums::Status }", Some("enums"))]
    // R25-SM3-01: the std crates and prelude names resolve everywhere.
    #[case::std_root_default(
        "struct D { #[serde(skip_serializing_if = \"std::option::Option::is_none\")] id: Option<i32> }",
        None
    )]
    #[case::std_prelude_default(
        "struct D { #[serde(default = \"Default::default\")] id: i32 }",
        None
    )]
    #[case::std_field_type("struct D { id: std::option::Option<i32> }", None)]
    // R29-SM4-01: `Decimal` is no prelude name, so the call site must resolve
    // it like any other imported name.
    #[case::decimal_is_judged("struct D { total: Decimal }", Some("Decimal"))]
    // R34-SM4-L1: an array length and a const generic argument name a const
    // the call site must resolve, judged by its root.
    #[case::array_length_const("struct D { digest: [u8; HASH_LEN] }", Some("HASH_LEN"))]
    #[case::const_generic_argument(
        "struct D { digest: std::marker::PhantomData<{ HASH_LEN }> }",
        Some("HASH_LEN")
    )]
    // A serde value that is no string literal is no copied path.
    #[case::non_string_value("struct D { #[serde(with = codec)] id: i32 }", None)]
    // R30-SM3-L4: a compiled-out field's type and serde path are not copied.
    #[case::cfg_off_field_type("struct D { #[cfg(false)] total: Decimal }", None)]
    #[case::cfg_off_serde_path(
        "struct D { #[cfg(false)] #[serde(with = \"codec::parse\")] id: i32 }",
        None
    )]
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

    /// R28-SM3-01: a DTO module that names the type only through a non-crate
    /// glob (`use sea_orm::entity::prelude::*;`, which `resolve_name` never
    /// pins) does not resolve it to the call site's item, so the copied name is
    /// refused. Resolved on both sides to the same item it is not.
    #[test]
    fn dto_side_glob_name_is_refused_when_the_call_site_resolves_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (file, text) in [
            ("lib.rs", "pub mod user;\npub mod routes;\npub mod types;"),
            ("user.rs", "use sea_orm::entity::prelude::*;"),
            ("routes.rs", "use crate::types::Uuid;"),
            ("types.rs", "pub struct Uuid;"),
        ] {
            std::fs::write(src.join(file), text).unwrap();
        }
        let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        let _call = set_test_call_site_module("crate::routes");
        let dto: syn::ItemStruct = syn::parse_str("struct D { id: Uuid }").unwrap();
        let dto_in = |module: &[&str]| module.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            unresolved_at_call_site(&dto, Some(&dto_in(&["crate", "user"]))),
            Some("Uuid".to_string())
        );
        assert_eq!(
            unresolved_at_call_site(&dto, Some(&dto_in(&["crate", "routes"]))),
            None
        );
    }

    /// R30-SM3-L2: the wrapper writes the adapter by its bare name, so a call
    /// site whose `Owner` is another item is refused. The stored item (also
    /// through an import of it) and a name the call site does not pin pass.
    #[rstest::rstest]
    #[case::other_module_item("use crate::api::Owner;", true)]
    #[case::stored_item("use crate::models::card::Owner;", false)]
    #[case::unpinned_name("", false)]
    fn adapter_name_must_name_the_stored_item(#[case] import: &str, #[case] refused: bool) {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        for (file, text) in [
            ("lib.rs", "pub mod api;\npub mod models;\npub mod routes;\n"),
            ("api.rs", "pub struct Owner { pub id: i32 }\n"),
            ("models.rs", "pub mod card;\n"),
            ("models/card.rs", "pub struct Owner { pub id: i32 }\n"),
            ("routes.rs", import),
        ] {
            let path = src.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        let _call = set_test_call_site_module("crate::routes");
        let dto: syn::ItemStruct = syn::parse_str("struct Owner { pub id: i32 }").unwrap();
        let adapter = syn::Ident::new("Owner", proc_macro2::Span::call_site());
        let module = ["crate", "models", "card"].map(String::from);
        let result = check_storage_fallback("owner", &adapter, "Owner", &dto, Some(&module));
        assert_eq!(result.is_err(), refused, "{result:?}");
        if refused {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("names another item"), "{error}");
        }
    }

    /// R30-SM3-L3: with the DTO's module unknown, only `pub` / `pub(crate)`
    /// fields are readable; an inherited or `pub(super)` one is refused.
    #[rstest::rstest]
    #[case::inherited("struct D { id: i32 }", Some("id"))]
    #[case::pub_super("struct D { pub(super) id: i32 }", Some("id"))]
    #[case::public("struct D { pub id: i32 }", None)]
    #[case::crate_visible("struct D { pub(crate) id: i32 }", None)]
    fn unknown_dto_module_admits_only_public_fields(
        #[case] src: &str,
        #[case] refused: Option<&str>,
    ) {
        let _call = set_test_call_site_module("crate::routes");
        let dto: syn::ItemStruct = syn::parse_str(src).unwrap();
        let adapter = syn::Ident::new("D", proc_macro2::Span::call_site());
        let result = check_storage_fallback("owner", &adapter, "D", &dto, None);
        match refused {
            Some(field) => {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(&format!("field `{field}`")), "{error}");
            }
            None => assert!(result.is_ok()),
        }
    }
}
