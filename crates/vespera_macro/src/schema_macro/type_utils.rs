//! Type utility functions for schema macro
//!
//! Provides helper functions for type analysis and manipulation.

use proc_macro2::TokenStream;
use quote::quote;
use serde_json;
use syn::{GenericArgument, PathArguments, Type};

use super::file_lookup::call_site_module;
use super::module_scope::{
    ModuleScope, Resolved, module_scope, path_readable_from, path_tokens, resolve_name,
    segment_ident,
};

/// SeaORM relation wrapper kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeaOrmRelationKind {
    /// `HasOne<T>` relation.
    HasOne,
    /// `HasMany<T>` relation.
    HasMany,
    /// `BelongsTo<T>` relation.
    BelongsTo,
}

impl SeaOrmRelationKind {
    /// Whether the relation is FK-backed on the current model.
    #[inline]
    pub const fn is_fk_backed(self) -> bool {
        matches!(self, Self::HasOne | Self::BelongsTo)
    }
}

/// Return the final path segment for path-like types.
#[inline]
pub fn last_path_segment(ty: &Type) -> Option<&syn::PathSegment> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    type_path.path.segments.last()
}

/// Return the first generic type argument on a path segment.
#[inline]
pub fn first_generic_type_arg(segment: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Inspect a `syn` type and return the SeaORM relation kind if its final path
/// segment is one of the supported relation wrappers.
pub fn seaorm_relation_kind(ty: &Type) -> Option<SeaOrmRelationKind> {
    let segment = last_path_segment(ty)?;
    if segment.ident == "HasOne" {
        Some(SeaOrmRelationKind::HasOne)
    } else if segment.ident == "HasMany" {
        Some(SeaOrmRelationKind::HasMany)
    } else if segment.ident == "BelongsTo" {
        Some(SeaOrmRelationKind::BelongsTo)
    } else {
        None
    }
}

/// Extract the inner target type of a SeaORM relation wrapper.
pub fn seaorm_relation_inner_type(ty: &Type) -> Option<&Type> {
    let segment = last_path_segment(ty)?;
    seaorm_relation_kind(ty)?;
    first_generic_type_arg(segment)
}

/// Primitive type names shared across parser and schema-macro type parsing.
pub const PRIMITIVE_TYPE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "String", "Decimal",
];

/// Normalize a `TokenStream` or `Type` to a compact string by removing whitespace.
#[inline]
pub fn normalize_token_str(displayable: &impl std::fmt::Display) -> String {
    let s = displayable.to_string();
    // Allocation profile: the `to_string` is unavoidable (`Display` -> owned
    // `String`); a second allocation happens only when whitespace is actually
    // present and must be stripped.  The fast-path gate scans raw bytes rather
    // than chars — every ASCII whitespace byte is a standalone code unit in
    // valid UTF-8, so the byte scan is equivalent to a char scan but skips the
    // per-char UTF-8 decode on the common (whitespace-free) path.
    if s.bytes().any(|b| b.is_ascii_whitespace()) {
        s.replace(|c: char| c.is_ascii_whitespace(), "")
    } else {
        s
    }
}

/// Extract type name from a Type
pub fn extract_type_name(ty: &Type) -> Result<String, syn::Error> {
    match ty {
        Type::Path(type_path) => {
            // Get the last segment (handles paths like crate::User)
            let segment = type_path.path.segments.last().ok_or_else(|| syn::Error::new_spanned(ty, "extract_type_name: type path has no segments. Provide a valid type like `User` or `crate::models::User`."))?;
            Ok(segment.ident.to_string())
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "extract_type_name: expected a type path, not a reference or other type. Use a type like `User` or `crate::User` instead of `&User`.",
        )),
    }
}

/// Extract the inner `T` from `Option<T>`.
///
/// Uses the last path segment so qualified forms such as
/// `std::option::Option<T>` and `core::option::Option<T>` are treated the same
/// as a bare `Option<T>`.
pub fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Check if a type is `Option<T>`.
pub fn is_option_type(ty: &Type) -> bool {
    option_inner(ty).is_some()
}

/// Check if a type is a `SeaORM` relation type (`HasOne`, `HasMany`, `BelongsTo`)
pub fn is_seaorm_relation_type(ty: &Type) -> bool {
    seaorm_relation_kind(ty).is_some()
}

/// Check if a struct is a `SeaORM` Model (has #[`sea_orm::model`] or #[`sea_orm(table_name` = ...)] attribute)
pub fn is_seaorm_model(struct_item: &syn::ItemStruct) -> bool {
    for attr in &struct_item.attrs {
        // Check for #[sea_orm::model] or #[sea_orm(...)]
        let path = attr.path();
        if path.is_ident("sea_orm") {
            return true;
        }
        // Check for path like sea_orm::model
        let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if segments.first().is_some_and(|s| s == "sea_orm") {
            return true;
        }
    }
    false
}

/// Check if a type name is a primitive or well-known type that doesn't need path resolution.
pub fn is_primitive_or_known_type(name: &str) -> bool {
    matches!(
        name,
        // Rust primitives
        "bool"
            | "char"
            | "str"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            // Common std types
            | "String"
            | "Vec"
            | "Option"
            | "Result"
            | "Box"
            | "Rc"
            | "Arc"
            | "HashMap"
            | "HashSet"
            | "BTreeMap"
            | "BTreeSet"
            // Chrono types
            | "DateTime"
            | "NaiveDateTime"
            | "NaiveDate"
            | "NaiveTime"
            | "Utc"
            | "Local"
            | "FixedOffset"
            // SeaORM types (will be converted separately)
            | "DateTimeWithTimeZone"
            | "DateTimeUtc"
            | "DateTimeLocal"
            | "Date"  // SeaORM re-export of chrono::NaiveDate
            | "Time"  // SeaORM re-export of chrono::NaiveTime
            // UUID
            | "Uuid"
            // Decimal (rust_decimal / sea_orm re-export)
            | "Decimal"
            // Serde JSON
            | "Value"
    )
}

/// Re-express `ty`, written inside the module `source_module_path`, so it
/// names the same type from ANY module of the crate (the generated struct
/// lives at the macro call site, not next to its source):
///
/// - names the module imports with `use` resolve through the import, never
///   through the module path (a private import re-exported that way is
///   `E0603`), locally defined items through the module path, and names a
///   crate-internal glob import brings in through that glob's module;
/// - relative multi-segment paths (`enums::Status`, `super::x::Y`) are
///   re-rooted; external crate paths stay as written;
/// - generic arguments are resolved recursively (`Option<Vec<T>>`,
///   `HashMap<K, V>`, references, tuples, arrays);
/// - SeaORM / chrono aliases become `vespera::chrono` types and the
///   `serde_json::Value` aliases (`Json`, `JsonValue`) become
///   `vespera::serde_json::Value` — unless the module defines or imports a
///   type of that name itself. A bare `Value` under a SeaORM glob import
///   (`use sea_orm::prelude::*;`) is the prelude's `Value`, written as
///   `sea_orm::prelude::Value`, not `serde_json::Value` (SM4-M1).
pub fn resolve_type_to_absolute_path(ty: &Type, source_module_path: &[String]) -> TokenStream {
    resolve_type_with_params(ty, source_module_path, &[])
}

/// [`resolve_type_to_absolute_path`] that leaves the listed generic
/// parameter names untouched (the source struct's own `T`, ...).
pub fn resolve_type_with_params(
    ty: &Type,
    source_module_path: &[String],
    generic_params: &[String],
) -> TokenStream {
    let scope = module_scope(source_module_path);
    TypeResolver {
        module: source_module_path,
        scope: scope.as_deref(),
        generic_params,
        call_site: call_site_module(),
    }
    .ty(ty)
}

struct TypeResolver<'a> {
    module: &'a [String],
    scope: Option<&'a ModuleScope>,
    generic_params: &'a [String],
    /// The module the emitted type is written in (`None`: unknown).
    call_site: Option<Vec<String>>,
}

impl TypeResolver<'_> {
    fn ty(&self, ty: &Type) -> TokenStream {
        match ty {
            Type::Path(type_path) if type_path.qself.is_none() => {
                self.path(&type_path.path).unwrap_or_else(|| quote! { #ty })
            }
            Type::Path(syn::TypePath {
                qself: Some(qself),
                path,
                ..
            }) => self.qself_path(qself, path),
            Type::Reference(reference) => {
                let lifetime = &reference.lifetime;
                let mutability = &reference.mutability;
                let elem = self.ty(&reference.elem);
                quote! { & #lifetime #mutability #elem }
            }
            Type::Slice(slice) => {
                let elem = self.ty(&slice.elem);
                quote! { [#elem] }
            }
            Type::Array(array) => {
                let elem = self.ty(&array.elem);
                let len = self.array_len(&array.len);
                quote! { [#elem; #len] }
            }
            Type::Tuple(tuple) => {
                let elems: Vec<TokenStream> = tuple.elems.iter().map(|e| self.ty(e)).collect();
                if elems.len() == 1 {
                    quote! { (#(#elems),*,) }
                } else {
                    quote! { (#(#elems),*) }
                }
            }
            Type::Paren(paren) => {
                let elem = self.ty(&paren.elem);
                quote! { (#elem) }
            }
            _ => quote! { #ty },
        }
    }

    /// A qualified-self path (`<user::Id as keys::Keyed>::Out`, R36-SM4-L2):
    /// the self type and the trait path (the first `position` segments) are
    /// re-rooted in the source module; the associated segments stay as written.
    fn qself_path(&self, qself: &syn::QSelf, path: &syn::Path) -> TokenStream {
        let self_ty = self.ty(&qself.ty);
        let position = qself.position.min(path.segments.len());
        let rest = path.segments.iter().skip(position);
        if position == 0 {
            return quote! { < #self_ty > :: #(#rest)::* };
        }
        let trait_path = syn::Path {
            leading_colon: path.leading_colon,
            segments: path.segments.iter().take(position).cloned().collect(),
        };
        let trait_path = self
            .path(&trait_path)
            .unwrap_or_else(|| quote! { #trait_path });
        quote! { < #self_ty as #trait_path > :: #(#rest)::* }
    }

    /// An array length: every constant the source module defines or imports
    /// (`[u8; HASH_LEN]`, `[u8; HASH_LEN * 2]`) is re-rooted like a type (it
    /// was copied as written: E0425 at the call site); a const generic
    /// parameter, a literal or an unknown name stays as written.
    fn array_len(&self, len: &syn::Expr) -> TokenStream {
        let mut len = len.clone();
        syn::visit_mut::VisitMut::visit_expr_mut(&mut LenPaths(self), &mut len);
        quote! { #len }
    }

    /// One path of an array length, resolved in the source module.
    fn len_path(&self, path: &syn::ExprPath) -> Option<TokenStream> {
        let plain = path.qself.is_none()
            && path.path.leading_colon.is_none()
            && path.path.segments.iter().all(|s| s.arguments.is_none());
        if !plain
            || path
                .path
                .get_ident()
                .is_some_and(|ident| self.generic_params.contains(&strip_raw(ident)))
        {
            return None;
        }
        let segments: Vec<String> = path
            .path
            .segments
            .iter()
            .map(|segment| strip_raw(&segment.ident))
            .collect();
        match resolve_name(&segments, self.module) {
            Resolved::Path(resolved) => Some(path_tokens(&resolved)),
            Resolved::Unknown => None,
        }
    }

    fn args(&self, args: &PathArguments) -> TokenStream {
        match args {
            PathArguments::None => TokenStream::new(),
            PathArguments::AngleBracketed(angle) => {
                let items = angle.args.iter().map(|arg| match arg {
                    GenericArgument::Type(inner) => self.ty(inner),
                    // A const argument (`Digest<{ HASH_LEN }>`) is re-rooted
                    // like an array length (R20-S4-L3, round 20).
                    GenericArgument::Const(expr) => self.array_len(expr),
                    other => quote! { #other },
                });
                let colon = &angle.colon2_token;
                quote! { #colon <#(#items),*> }
            }
            // `Fn(A) -> B` bounds: not a serializable field type.
            PathArguments::Parenthesized(_) => quote! { #args },
        }
    }

    fn path(&self, path: &syn::Path) -> Option<TokenStream> {
        let last = path.segments.last()?;
        if path.leading_colon.is_none() && path.segments.len() == 1 {
            return Some(self.single(last));
        }
        let name = strip_raw(&last.ident);
        if SEA_ORM_PATH_ALIASES.contains(&name.as_str())
            && self.names_alias_crate(path)
            && let Some(converted) = self.alias(&name, &last.arguments, true)
        {
            return Some(converted);
        }
        let mut segments: Vec<(String, TokenStream)> = path
            .segments
            .iter()
            .map(|segment| (strip_raw(&segment.ident), self.args(&segment.arguments)))
            .collect();
        if path.leading_colon.is_some() {
            segments[0].0.insert_str(0, "::");
        } else if !self.generic_params.contains(&segments[0].0) {
            // A generic parameter's path (`Item::Cursor` of `Page<Item>`) stays
            // as written: an import or item of the same name is not its root
            // (R27-SM4-L2, as `single` keeps a bare parameter).
            // The whole leading `self` / `super` run is resolved at once:
            // `super::super::x` resolved one `super` at a time left the second
            // one in the emitted path (R20-S4-M1, round 20).
            let run = leading_run_len(&segments);
            let names: Vec<String> = segments[..run]
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            if let Resolved::Path(prefix) = resolve_name(&names, self.module) {
                let first_args = segments
                    .drain(..run)
                    .next_back()
                    .map(|(_, args)| args)
                    .unwrap_or_default();
                let mut rooted: Vec<(String, TokenStream)> = prefix
                    .into_iter()
                    .map(|s| (s, TokenStream::new()))
                    .collect();
                // A `use ::a::b;` import resolves to `["", "a", "b"]`: its empty
                // root becomes the `::` prefix of the next segment (an empty
                // segment has no identifier).
                if rooted.first().is_some_and(|(name, _)| name.is_empty()) {
                    rooted.remove(0);
                    if let Some(next) = rooted.first_mut() {
                        next.0.insert_str(0, "::");
                    }
                }
                if let Some(last) = rooted.last_mut() {
                    last.1 = first_args;
                }
                rooted.extend(segments);
                segments = rooted;
            }
        }
        let names: Vec<String> = segments.iter().map(|(name, _)| name.clone()).collect();
        if names.first().is_some_and(|first| first == "crate")
            && !path_readable_from(&names, self.call_site.as_deref())
        {
            let owner = &names[..names.len() - 1];
            return Some(unreadable(&last.ident, owner));
        }
        Some(render_segments(&segments))
    }

    fn single(&self, segment: &syn::PathSegment) -> TokenStream {
        let name = strip_raw(&segment.ident);
        let ident = &segment.ident;
        let args = self.args(&segment.arguments);
        if self.generic_params.contains(&name) {
            return quote! { #ident #args };
        }
        if let Some(scope) = self.scope {
            if let Some(import) = scope.imports.get(&name) {
                if first_named(import) == Some("sea_orm")
                    && let Some(converted) = self.alias(&name, &segment.arguments, true)
                {
                    return converted;
                }
                // Through a glob-imported first segment too (`resolve_name`).
                let resolved = resolve_name(std::slice::from_ref(&name), self.module);
                let path = path_tokens(if let Resolved::Path(path) = &resolved {
                    path
                } else {
                    import
                });
                return quote! { #path #args };
            }
            if scope.locals.contains(&name) {
                if !scope.item_readable_from(&name, self.call_site.as_deref()) {
                    return unreadable(ident, self.module);
                }
                return self.module_qualified(ident, &args);
            }
        }
        // A crate-internal glob naming it (`use crate::types::*;` defining
        // `Value`) wins over the alias, like an explicit import does.
        if let Resolved::Path(path) = resolve_name(std::slice::from_ref(&name), self.module) {
            if first_named(&path) == Some("sea_orm")
                && let Some(converted) = self.alias(&name, &segment.arguments, true)
            {
                return converted;
            }
            let path = path_tokens(&path);
            return quote! { #path #args };
        }
        let sea_orm_scope = self.scope.is_some_and(ModuleScope::has_sea_orm_glob);
        let sea_orm_alias = self.alias(&name, &segment.arguments, sea_orm_scope);
        let sea_orm_glob = self.scope.and_then(|scope| {
            scope
                .globs
                .iter()
                .find(|glob| first_named(glob) == Some("sea_orm"))
        });
        // A bare SeaORM prelude name with a SeaORM glob in scope is the SeaORM
        // one, not an external glob's (`use std::collections::*;` took `Uuid`,
        // R21-B4-M2, round 21). Only the names the prelude exports count: a
        // chrono alias it does not export (`Local`) stays the glob's
        // (R22-SM4-M1).
        let sea_orm_prelude_name = sea_orm_glob.is_some()
            && segment.arguments.is_none()
            && SEA_ORM_PRELUDE_NAMES.contains(&name.as_str());
        let several_external_globs = self
            .scope
            .is_some_and(|scope| external_globs(scope).count() > 1);
        // The chrono names of [`CHRONO_ONLY_NAMES`] have no other meaning a
        // glob could give them, so several chrono-family globs do not make
        // them ambiguous: they keep their chrono alias (R24-SM4-L1). Any other
        // external glob may export its own type of that name, so with one of
        // those the explicit-import error applies (R25-SM4-L1).
        let chrono_family_globs = self.scope.is_some_and(|scope| {
            external_globs(scope)
                .all(|glob| matches!(first_named(glob), Some("chrono" | "vespera" | "sea_orm")))
        });
        let chrono_only = several_external_globs
            && chrono_family_globs
            && segment.arguments.is_none()
            && CHRONO_ONLY_NAMES.contains(&name.as_str());
        // A name the source module neither defines nor imports, nor a std
        // prelude name, comes from one of its external glob imports (it
        // compiled there): emitted through that glob, not as a bare alias
        // (`use toml::*;` `Value` was `serde_json::Value`) (R20-S4-L1, round 20).
        if let Some(glob) = self.scope.and_then(external_glob)
            && !is_prelude_name(&name)
            && !sea_orm_prelude_name
            && !chrono_only
        {
            // Several external globs and no other meaning for the name: which
            // one supplies it would be a guess, so the user imports it
            // explicitly (R22-SM4-L1). A name a SeaORM alias also knows (`Utc`,
            // `Local`) takes the same path: its alias meaning is no reason to
            // pick one of the globs silently (R23-SM4-L1).
            if several_external_globs {
                return syn::Error::new(
                    ident.span(),
                    format!(
                        "schema_type!: `{name}` may come from several glob imports of this module; import it explicitly (`use <module>::{name};`)"
                    ),
                )
                .to_compile_error();
            }
            let base = path_tokens(glob);
            return quote! { #base :: #ident #args };
        }
        if let Some(converted) = sea_orm_alias {
            return converted;
        }
        if matches!(name.as_str(), "Uuid" | "Decimal")
            && let Some(glob) = sea_orm_glob
        {
            let base = path_tokens(glob);
            return quote! { #base :: #ident };
        }
        // A bare name the SeaORM prelude exports (`Value`) is the prelude's,
        // not the std / serde_json name the shortcut below would give it
        // (SM4-M1, round 38): the glob path comes first for those.
        if is_primitive_or_known_type(&name) && !sea_orm_prelude_name {
            return quote! { #ident #args };
        }
        // Any other name the module neither defines nor imports comes from
        // its SeaORM prelude glob (`TimeDateTime`, `BigDecimal`, `IpNetwork`,
        // ...): through the module it would be a private glob import.
        if let Some(glob) = sea_orm_glob {
            let base = path_tokens(glob);
            return quote! { #base :: #ident #args };
        }
        if self.module.is_empty() {
            return quote! { #ident #args };
        }
        self.module_qualified(ident, &args)
    }

    /// Whether a multi-segment `path` starts in a crate defining one of the
    /// [`SEA_ORM_PATH_ALIASES`] (written, or through an import of the
    /// module): a user's own `crate::upload::FieldData` stays as written.
    fn names_alias_crate(&self, path: &syn::Path) -> bool {
        let first = path
            .segments
            .first()
            .map(|segment| strip_raw(&segment.ident))
            .unwrap_or_default();
        let root = match resolve_name(std::slice::from_ref(&first), self.module) {
            Resolved::Path(resolved) if path.leading_colon.is_none() => {
                first_named(&resolved).map(str::to_owned)
            }
            _ => Some(first),
        };
        root.is_some_and(|root| ALIAS_CRATES.contains(&root.as_str()))
    }

    fn module_qualified(&self, ident: &syn::Ident, args: &TokenStream) -> TokenStream {
        let module = path_tokens(self.module);
        quote! { #module :: #ident #args }
    }

    /// SeaORM / chrono / `serde_json` / multipart aliases. `Date` / `Time`
    /// are SeaORM's only in a module glob-importing a SeaORM prelude (they
    /// are `time` crate names too).
    fn alias(&self, name: &str, args: &PathArguments, sea_orm_scope: bool) -> Option<TokenStream> {
        let bare = args.is_none();
        Some(match name {
            "DateTimeWithTimeZone" if bare => {
                quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
            }
            "DateTimeUtc" if bare => quote! { vespera::chrono::DateTime<vespera::chrono::Utc> },
            "DateTimeLocal" if bare => {
                quote! { vespera::chrono::DateTime<vespera::chrono::Local> }
            }
            // chrono's `DateTime` always takes a time zone: the bare name is
            // SeaORM's alias of `NaiveDateTime`.
            "DateTime" if bare => quote! { vespera::chrono::NaiveDateTime },
            // SeaORM's `Chrono*` prelude aliases (sea-orm 2).
            "ChronoDateTimeWithTimeZone" if bare && sea_orm_scope => {
                quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
            }
            "ChronoDateTimeUtc" if bare && sea_orm_scope => {
                quote! { vespera::chrono::DateTime<vespera::chrono::Utc> }
            }
            "ChronoDateTimeLocal" if bare && sea_orm_scope => {
                quote! { vespera::chrono::DateTime<vespera::chrono::Local> }
            }
            "ChronoDateTime" if bare && sea_orm_scope => quote! { vespera::chrono::NaiveDateTime },
            "ChronoUtc" if bare && sea_orm_scope => quote! { vespera::chrono::Utc },
            "DateTime" => {
                let args = self.args(args);
                quote! { vespera::chrono::DateTime #args }
            }
            "Date" | "ChronoDate" if bare && sea_orm_scope => quote! { vespera::chrono::NaiveDate },
            "Time" | "ChronoTime" if bare && sea_orm_scope => quote! { vespera::chrono::NaiveTime },
            "NaiveDateTime" | "NaiveDate" | "NaiveTime" | "Utc" | "Local" | "FixedOffset"
                if bare =>
            {
                let ident = syn::Ident::new(name, proc_macro2::Span::call_site());
                quote! { vespera::chrono::#ident }
            }
            "Json" | "JsonValue" if bare => quote! { vespera::serde_json::Value },
            // Under a SeaORM glob a bare `Value` is sea_query's `Value` (the
            // prelude's), not `serde_json::Value`.
            "Value" if bare && !sea_orm_scope => quote! { vespera::serde_json::Value },
            "FieldData" => {
                let args = self.args(args);
                quote! { vespera::multipart::FieldData #args }
            }
            "NamedTempFile" => quote! { vespera::tempfile::NamedTempFile },
            _ => return None,
        })
    }
}

/// Aliases converted whatever path spells them (`sea_orm::prelude::DateTimeUtc`).
const SEA_ORM_PATH_ALIASES: &[&str] = &[
    "DateTimeWithTimeZone",
    "DateTimeUtc",
    "DateTimeLocal",
    "FieldData",
    "NamedTempFile",
];

/// The crates whose paths may spell one of [`SEA_ORM_PATH_ALIASES`].
const ALIAS_CRATES: &[&str] = &["sea_orm", "vespera", "axum_typed_multipart", "tempfile"];

/// chrono names the bare alias maps to `vespera::chrono` whatever the globs
/// import: no other crate's common type carries them (R24-SM4-L1).
const CHRONO_ONLY_NAMES: &[&str] = &["NaiveDate", "NaiveDateTime", "NaiveTime", "FixedOffset"];

/// The names sea-orm's `prelude` exports that a bare SeaORM-glob name can
/// resolve to (the aliases above, `Uuid`, `Decimal`, ..). `Date` / `Time` /
/// `DateTime` are exported only with sea-orm's `with-chrono` feature, which
/// the macro cannot see, so they are taken as exported.
const SEA_ORM_PRELUDE_NAMES: &[&str] = &[
    "Value",
    "Json",
    "Date",
    "Time",
    "DateTime",
    "DateTimeWithTimeZone",
    "DateTimeUtc",
    "DateTimeLocal",
    "ChronoDate",
    "ChronoTime",
    "ChronoDateTime",
    "ChronoDateTimeWithTimeZone",
    "ChronoDateTimeUtc",
    "ChronoUtc",
    "ChronoDateTimeLocal",
    "Uuid",
    "Decimal",
    "BigDecimal",
];

/// The error for a name the emitted type cannot read: an item of `owner` that
/// is private there, named from the call site (`E0603`).
fn unreadable(ident: &syn::Ident, owner: &[String]) -> TokenStream {
    syn::Error::new(
        ident.span(),
        format!(
            "schema_type!: `{}` is private to `{}`, so the DTO emitted elsewhere cannot name it: make it `pub(crate)` or declare the DTO in that module",
            strip_raw(ident),
            owner.join("::"),
        ),
    )
    .to_compile_error()
}

/// Re-roots every path of an array length expression ([`TypeResolver::len_path`]).
struct LenPaths<'r, 'a>(&'r TypeResolver<'a>);

impl syn::visit_mut::VisitMut for LenPaths<'_, '_> {
    fn visit_expr_mut(&mut self, expr: &mut syn::Expr) {
        if let syn::Expr::Path(path) = expr
            && let Some(rerooted) = self.0.len_path(path)
        {
            *expr = syn::Expr::Verbatim(rerooted);
            return;
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }
}

/// How many leading segments of a type path resolve together: its leading
/// `self` / `super` run plus the name after it (`self::Status` must consult
/// that module's imports, R29-SM3-L1), or the first segment alone (at least
/// one).
fn leading_run_len(segments: &[(String, TokenStream)]) -> usize {
    let relative = segments
        .iter()
        .take_while(|(name, _)| name == "self" || name == "super")
        .count();
    (relative + usize::from(relative > 0)).clamp(1, segments.len())
}

/// The external glob imports of `scope`, in source order: not `crate` /
/// `self` / `super` (resolved by name) and not a crate [`ALIAS_CRATES`] names
/// (its aliases).
fn external_globs(scope: &ModuleScope) -> impl Iterator<Item = &Vec<String>> {
    scope.globs.iter().filter(|glob| {
        first_named(glob).is_some_and(|root| {
            !matches!(root, "crate" | "self" | "super") && !ALIAS_CRATES.contains(&root)
        })
    })
}

/// The first of [`external_globs`].
fn external_glob(scope: &ModuleScope) -> Option<&Vec<String>> {
    external_globs(scope).next()
}

/// Names a glob import never supplies: the primitives and the std prelude.
/// `Decimal` is in [`PRIMITIVE_TYPE_NAMES`] for the schema mapping only; it is
/// no primitive and no prelude name, so an external glob (`use rust_decimal::*;`)
/// supplies it (R29-SM4-01).
pub(super) fn is_prelude_name(name: &str) -> bool {
    (PRIMITIVE_TYPE_NAMES.contains(&name) && name != "Decimal")
        // `char` and `str` are primitives the list above does not carry: a glob
        // import would have been taken for them (R21-B4-M1, round 21).
        || matches!(
            name,
            "char" | "str" | "Vec" | "Option" | "Result" | "Box" | "Self"
        )
}

fn first_named(path: &[String]) -> Option<&str> {
    path.iter().find(|s| !s.is_empty()).map(String::as_str)
}

fn strip_raw(ident: &syn::Ident) -> String {
    crate::parser::strip_raw_prefix_owned(ident.to_string())
}

fn render_segments(segments: &[(String, TokenStream)]) -> TokenStream {
    let mut out = TokenStream::new();
    for (index, (name, args)) in segments.iter().enumerate() {
        let name = if let Some(rest) = name.strip_prefix("::") {
            out.extend(quote! { :: });
            rest
        } else {
            if index > 0 {
                out.extend(quote! { :: });
            }
            name.as_str()
        };
        let ident = segment_ident(name);
        out.extend(quote! { #ident #args });
    }
    out
}

// Extract the module path from a type (excluding the type name itself).
/// e.g., `crate::models::memo::Model` -> `["crate", "models", "memo"]`
pub fn extract_module_path(ty: &Type) -> Vec<String> {
    match ty {
        Type::Path(type_path) => {
            let segments: Vec<String> = type_path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            // Return all but the last segment (which is the type name)
            if segments.len() > 1 {
                segments[..segments.len() - 1].to_vec()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// Capitalize the first letter of a string.
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Convert `snake_case` to `PascalCase`.
/// e.g., "`target_user_id`" -> "`TargetUserId`", "comments" -> "Comments"
pub fn snake_to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

/// Check if a type is `HashMap` or `BTreeMap`
pub fn is_map_type(ty: &Type) -> bool {
    // `segments.last()` yields `None` for an empty path, so the let-chain
    // both replaces the prior `is_empty()` guard + `unwrap()` and skips the
    // per-call `ident.to_string()` allocation (`Ident: PartialEq<str>`).
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        return segment.ident == "HashMap" || segment.ident == "BTreeMap";
    }
    false
}

/// Check if a type is a primitive type OR a known well-behaved container.
///
/// This checks the outer type name against a list of known types (primitives, std containers, etc.).
/// Types like `Vec`, `Option`, `HashMap` are considered primitive-like regardless of their contents.
pub fn is_primitive_like(ty: &Type) -> bool {
    is_primitive_or_known_type(&extract_type_name(ty).unwrap_or_default())
}

/// Names whose default below is the value of an ecosystem type (SeaORM /
/// chrono / rust_decimal / uuid), which a crate may also define itself.
const ECOSYSTEM_DEFAULT_NAMES: &[&str] = &[
    "Decimal",
    "Uuid",
    "Date",
    "Time",
    "DateTime",
    "DateTimeUtc",
    "DateTimeWithTimeZone",
    "NaiveDate",
    "NaiveTime",
    "NaiveDateTime",
];

/// Whether the path `segments`, written in the module `source_module` (the
/// module of the struct the field is declared in), names an ecosystem type.
/// A crate-local type of the same name (`struct Decimal(i64)`) advertised
/// `"0"` / `"1970-01-01"` before. Without a known module (`source_module` is
/// empty) the name is taken as the ecosystem type, as before.
fn names_ecosystem_type(segments: &[String], source_module: &[String]) -> bool {
    use super::module_scope::{Resolved, resolve_name};
    let mut path = match resolve_name(segments, source_module) {
        Resolved::Path(path) => path,
        Resolved::Unknown => return true,
    };
    // A crate-internal re-export (`pub use rust_decimal::Decimal;` in
    // `crate::types`, reached through `use crate::types::Decimal;`) names the
    // external root one module further in: follow the chain (R34-SM4-L2).
    for _ in 0..8 {
        let Some((name, module)) = path
            .split_last()
            .filter(|(_, module)| module.first().map(String::as_str) == Some("crate"))
        else {
            break;
        };
        match resolve_name(std::slice::from_ref(name), module) {
            Resolved::Path(next) if next != path => path = next,
            _ => break,
        }
    }
    matches!(
        path.first().map(String::as_str),
        Some("sea_orm" | "chrono" | "rust_decimal" | "uuid" | "time")
    )
}

/// Get type-specific default value for simple #[serde(default)].
///
/// `source_module` is the absolute module (`crate::..`) the field's type is
/// written in; it resolves an ecosystem-looking name (`Decimal`, `Date`).
/// Empty when unknown.
pub fn get_type_default(ty: &Type, source_module: &[String]) -> Option<serde_json::Value> {
    match ty {
        Type::Path(type_path) => type_path.path.segments.last().and_then(|segment| {
            let name = segment.ident.to_string();
            if ECOSYSTEM_DEFAULT_NAMES.contains(&name.as_str()) {
                let segments: Vec<String> = type_path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect();
                if !names_ecosystem_type(&segments, source_module) {
                    return None;
                }
            }
            match name.as_str() {
                "String" => Some(serde_json::Value::String(String::new())),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Some(serde_json::Value::Number(serde_json::Number::from(0))),
                "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => {
                    Some(serde_json::Value::Array(Vec::new()))
                }
                "HashMap" | "BTreeMap" => Some(serde_json::Value::Object(serde_json::Map::new())),
                // `rust_decimal` serializes as a string.
                "Decimal" => Some(serde_json::Value::String("0".to_string())),
                "f32" | "f64" => Some(serde_json::Value::Number(
                    serde_json::Number::from_f64(0.0)
                        .unwrap_or_else(|| serde_json::Number::from(0)),
                )),
                "bool" => Some(serde_json::Value::Bool(false)),
                "Option" => Some(serde_json::Value::Null),
                "Uuid" => Some(serde_json::Value::String(
                    "00000000-0000-0000-0000-000000000000".to_string(),
                )),
                // chrono's epoch as each zone serializes it (`Local` depends
                // on the machine: no documented value).
                "DateTimeUtc" => Some(serde_json::Value::String(
                    "1970-01-01T00:00:00Z".to_string(),
                )),
                "DateTimeWithTimeZone" => Some(serde_json::Value::String(
                    "1970-01-01T00:00:00+00:00".to_string(),
                )),
                "DateTime" => match first_generic_type_arg(segment)
                    .and_then(last_path_segment)
                    .map(|zone| zone.ident.to_string())
                    .as_deref()
                {
                    Some("Utc") => Some(serde_json::Value::String(
                        "1970-01-01T00:00:00Z".to_string(),
                    )),
                    Some("FixedOffset") => Some(serde_json::Value::String(
                        "1970-01-01T00:00:00+00:00".to_string(),
                    )),
                    // The bare name is SeaORM's `NaiveDateTime` alias.
                    None if segment.arguments.is_none() => {
                        Some(serde_json::Value::String("1970-01-01T00:00:00".to_string()))
                    }
                    _ => None,
                },
                "NaiveDateTime" => {
                    Some(serde_json::Value::String("1970-01-01T00:00:00".to_string()))
                }
                // `Date` is SeaORM's `NaiveDate` alias.
                "NaiveDate" | "Date" => Some(serde_json::Value::String("1970-01-01".to_string())),
                "NaiveTime" | "Time" => Some(serde_json::Value::String("00:00:00".to_string())),
                _ => None,
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_glob;
