//! Route attribute macro implementation.
//!
//! This module implements the `#[vespera::route]` attribute macro that validates
//! and processes handler functions for route registration.
//!
//! # Overview
//!
//! The `#[route]` attribute is applied to handler functions to:
//! - Validate that the function is `pub async fn`
//! - Parse route configuration (HTTP method, path, tags, etc.)
//! - Mark the function for route discovery by the `vespera!` macro
//!
//! # Route Requirements
//!
//! All handler functions must:
//! - Be public (`pub`)
//! - Be async (`async fn`)
//! - Accept standard Axum extractors (Path, Query, Json, etc.)
//! - Return a response type (Json, String, `StatusCode`, etc.)
//!
//! # Key Functions
//!
//! - [`validate_route_fn`] - Validate route function signature
//! - [`process_route_attribute`] - Parse and process the route attribute
//!
//! # Example
//!
//! ```ignore
//! #[vespera::route(get, path = "/{id}", tags = ["users"])]
//! pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
//!     Json(User { id, name: "Alice".into() })
//! }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::macro_storage::{CrateStorage, SourceIdentified};
use crate::{args, metadata::HeaderParam};

mod generic_scope;
mod item_error;
mod path_check;

use item_error::route_item_error;

/// Metadata stored by `#[route]` for later consumption by `vespera!()`.
///
/// Each invocation of `#[route]` pushes one entry into [`ROUTE_STORAGE`].
/// The `vespera!()` macro reads this storage to supplement file-based
/// route discovery — when `file_path` is populated, the collector can
/// build route metadata directly from this struct without re-parsing
/// the source file with `syn::parse_file()`.
#[derive(Debug, Clone, Default)]
pub struct StoredRouteInfo {
    /// Function name (e.g., `"get_user"`).
    pub fn_name: String,
    /// HTTP method (e.g., `"get"`, `"post"`).  Used by the collector's
    /// fast path ([`crate::collector`]) to populate `RouteMetadata.method`
    /// without re-parsing the source file.
    pub method: Option<String>,
    /// Custom path from `path = "/{id}"`.  Used by the collector to
    /// derive the full route URL when present.
    pub custom_path: Option<String>,
    /// Declared non-200 success status from `status = <u16>` (validated 2xx).
    pub success_status: Option<u16>,
    /// Additional error status codes from `error_status = [400, 404]`.
    pub error_status: Option<Vec<u16>>,
    /// Typed error responses from `responses = [(404, NotFoundError)]`.
    pub typed_responses: Option<Vec<(u16, String)>>,
    /// Tags for `OpenAPI` grouping from `tags = ["users"]`.
    pub tags: Option<Vec<String>>,
    /// Per-route security requirements from `security = ["bearerAuth"]` /
    /// `security = [("oauth2", ["read:users"])]`.
    pub security: Option<Vec<vespera_core::schema::SecurityRequirement>>,
    /// Header parameters from `headers = [{ name = "Authorization" }]`.
    pub headers: Vec<HeaderParam>,
    /// Explicit OpenAPI operationId from `operation_id = "getUser"`.
    pub operation_id: Option<String>,
    /// OpenAPI operation summary from `summary = "Get user"`.
    pub summary: Option<String>,
    /// Operation-level request example.
    pub request_example: Option<serde_json::Value>,
    /// Operation-level response example.
    pub response_example: Option<serde_json::Value>,
    /// Whether the operation is deprecated via bare `deprecated`.
    pub deprecated: bool,
    /// Description from `description = "Get user by ID"`.
    pub description: Option<String>,
    /// Source file path from `Span::call_site().local_file()` (requires Rust 1.88+).
    /// `None` on older Rust — collector falls back to full file parsing.
    pub file_path: Option<String>,
    /// Function signature as a string. Re-parsed via `syn::parse_str()` by
    /// [`crate::openapi_generator`] when the source file AST is unavailable.
    /// Stores only `syn::Signature` tokens, not the handler body.
    pub fn_sig_str: String,
    /// Inline-module path of the handler inside `file_path` (`Some("")` at
    /// the file's top level, `Some("v2")` inside `mod v2 { .. }`; `None` when
    /// it could not be located).  Part of the storage key, so a top-level
    /// handler and a same-named one in an inline module never replace each
    /// other, and lets the collector reject an inline-module handler without
    /// re-parsing the file.
    pub inline_module: Option<String>,
}

/// Per-crate storage for route metadata collected by `#[route]` attribute
/// macros, read by `vespera!()` / `export_app!()` to supplement file-based
/// route discovery.
///
/// Keyed by [`crate::schema_impl::current_crate_key`] so a long-lived
/// rust-analyzer proc-macro server (one process, many crates) never feeds
/// crate A's routes into crate B's generated router/spec. See
/// [`SCHEMA_STORAGE`](crate::schema_impl::SCHEMA_STORAGE) for the rationale.
pub static ROUTE_STORAGE: CrateStorage<StoredRouteInfo> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl SourceIdentified for StoredRouteInfo {
    fn fn_name(&self) -> &str {
        &self.fn_name
    }
    fn file_path(&self) -> Option<&str> {
        self.file_path.as_deref()
    }
    fn scope(&self) -> Option<&str> {
        self.inline_module.as_deref()
    }
}

/// Replace-insert a `#[route]` metadata entry in the current crate's bucket.
pub fn register_route(info: StoredRouteInfo) {
    crate::macro_storage::register(&ROUTE_STORAGE, info);
}

/// Snapshot of the current crate's registered routes — a cheap `Arc` clone, so
/// consumers never deep-clone every stored function signature string.
#[must_use]
pub fn current_crate_routes() -> Arc<Vec<StoredRouteInfo>> {
    crate::macro_storage::current_crate_items(&ROUTE_STORAGE)
}

/// An HTTP status code literal of `#[route]` (`error_status` / `responses`):
/// an integer in `100..=599`, the range an OpenAPI response key can name.
/// Anything else (`0`, `999`, `70_000`) is a compile error with `message`.
fn status_code_literal(elem: &syn::Expr, lit_int: &syn::LitInt, message: &str) -> syn::Result<u16> {
    lit_int
        .base10_parse::<u16>()
        .ok()
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| syn::Error::new_spanned(elem, message))
}

/// Extract the `u16` error status codes of `error_status = [...]`.
///
/// Every entry must be an integer literal that names an HTTP status code
/// (`100..=599`). A dropped entry (a named constant, an out-of-range status)
/// would leave its response undocumented with no diagnostic, so it is a
/// compile error instead.
fn extract_error_status_codes(arr: &syn::ExprArray) -> syn::Result<Option<Vec<u16>>> {
    let codes = arr
        .elems
        .iter()
        .map(|elem| match elem {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(lit_int),
                ..
            }) => status_code_literal(
                elem,
                lit_int,
                "#[route] `error_status` entries must be HTTP status codes (integer literals in 100..=599)",
            ),
            _ => Err(syn::Error::new_spanned(
                elem,
                "#[route] `error_status` entries must be integer literals, e.g. `error_status = [400, 404]`",
            )),
        })
        .collect::<syn::Result<Vec<u16>>>()?;
    Ok((!codes.is_empty()).then_some(codes))
}

/// Reject any non-string-literal element of a `[...]` attribute array with a
/// spanned compile error instead of silently dropping it.  A typo such as
/// `security = [bearerAuth]` (missing quotes) or `tags = [users]` would
/// otherwise vanish — and for `security` that silently documents a PROTECTED
/// route as unauthenticated, so this guard is security-relevant.
fn require_string_literal_elems(arr: &syn::ExprArray, attr_name: &str) -> syn::Result<()> {
    for elem in &arr.elems {
        if !matches!(
            elem,
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            })
        ) {
            return Err(syn::Error::new_spanned(
                elem,
                format!(
                    "#[route] `{attr_name}` entries must be string literals, e.g. `{attr_name} = [\"name\"]`"
                ),
            ));
        }
    }
    Ok(())
}

/// Collect every `syn::Lit::Str` value from a validated array.  Call only after
/// [`require_string_literal_elems`] so non-string elements cannot slip through.
fn collect_string_literal_values(arr: &syn::ExprArray) -> Vec<String> {
    arr.elems
        .iter()
        .filter_map(|elem| {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = elem
            {
                Some(lit_str.value())
            } else {
                None
            }
        })
        .collect()
}

/// Extract `String` tags from a `syn::ExprArray`, erroring on any non-string
/// entry.  An all-empty / no-string array yields `None`.
fn extract_tag_strings(arr: &syn::ExprArray) -> syn::Result<Option<Vec<String>>> {
    require_string_literal_elems(arr, "tags")?;
    let tags = collect_string_literal_values(arr);
    Ok(if tags.is_empty() { None } else { Some(tags) })
}

fn parse_example_string(lit: &syn::LitStr) -> serde_json::Value {
    let value = lit.value();
    serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
}

/// Extract typed response status/schema pairs from `responses = [(404, NotFoundError)]`.
///
/// Every entry must be a `(status literal, schema path)` tuple: a malformed
/// entry is a compile error, not a response the document silently lacks.
fn extract_typed_responses(arr: &syn::ExprArray) -> syn::Result<Option<Vec<(u16, String)>>> {
    // A status listed twice would keep only the last schema: a compile error,
    // not a documented response that silently disappears.
    let mut seen = std::collections::HashSet::new();
    let responses = arr
        .elems
        .iter()
        .map(|elem| {
            let (status, schema) = typed_response_entry(elem)?;
            if !seen.insert(status) {
                return Err(syn::Error::new_spanned(
                    elem,
                    format!("#[route] `responses` lists status {status} more than once"),
                ));
            }
            Ok((status, schema))
        })
        .collect::<syn::Result<Vec<(u16, String)>>>()?;
    Ok((!responses.is_empty()).then_some(responses))
}

/// One `(status, schema)` entry of `responses = [...]`.
fn typed_response_entry(elem: &syn::Expr) -> syn::Result<(u16, String)> {
    const EXPECTED: &str = "#[route] `responses` entries must be `(status, SchemaType)` pairs, e.g. `responses = [(404, NotFoundError)]`";
    let syn::Expr::Tuple(tuple) = elem else {
        return Err(syn::Error::new_spanned(elem, EXPECTED));
    };
    // The arity is part of the shape: a third element would be dropped.
    if tuple.elems.len() != 2 {
        return Err(syn::Error::new_spanned(elem, EXPECTED));
    }
    let status = match tuple.elems.first() {
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(lit_int),
            ..
        })) => status_code_literal(
            elem,
            lit_int,
            "#[route] `responses` status codes must be integer literals in 100..=599",
        )?,
        _ => return Err(syn::Error::new_spanned(elem, EXPECTED)),
    };
    let schema_name = match tuple.elems.get(1) {
        Some(syn::Expr::Path(path)) => crate::route::response_type_path(&path.path),
        _ => None,
    }
    .ok_or_else(|| syn::Error::new_spanned(elem, EXPECTED))?;
    Ok((status, schema_name))
}

/// The signature stored for `vespera!`: the handler's own signature with a
/// return-type alias (`-> ApiResult<User>`) expanded through the aliases
/// visible from `source_file` / the crate's `src/` (see
/// [`crate::parser::expand_return_type_alias`]). The source is only read when
/// the return type could name an alias.
fn stored_signature(sig: &syn::Signature, source_file: Option<&std::path::Path>) -> syn::Signature {
    let mut sig = sig.clone();
    if let Some(file) = source_file.filter(|_| crate::parser::needs_alias_expansion(&sig)) {
        use crate::schema_macro::file_cache;
        // Only item names, `use` trees and inline modules are read: the
        // handler bodies are skipped.
        let same_file = file_cache::get_shallow_parsed_file(file);
        let src_dir =
            file_cache::get_manifest_dir().map(|dir| std::path::PathBuf::from(dir).join("src"));
        crate::parser::expand_return_type_alias(
            &mut sig,
            Some(file),
            same_file.as_ref(),
            src_dir.as_deref(),
        );
    }
    sig
}

/// A `#[route]` handler parsed only as far as route registration needs it:
/// outer attributes, visibility and signature.
///
/// The body is checked to be a single `{ ... }` group but its tokens are
/// **not** parsed into a `syn::Block` — nothing reads it (the item is
/// re-emitted verbatim), and building the statement/expression tree of every
/// handler body was the bulk of `#[route]`'s expansion time. A syntax error
/// inside the body is still reported by rustc when it parses the re-emitted
/// item.
pub struct RouteFnHead {
    pub attrs: Vec<syn::Attribute>,
    pub vis: syn::Visibility,
    pub sig: syn::Signature,
}

impl syn::parse::Parse for RouteFnHead {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_outer)?;
        let vis = input.parse()?;
        let sig = input.parse()?;
        let body;
        syn::braced!(body in input);
        body.parse::<proc_macro2::TokenStream>()?;
        Ok(Self { attrs, vis, sig })
    }
}

/// Validate route function - must be pub and async
pub fn validate_route_fn(item_fn: &RouteFnHead) -> Result<(), syn::Error> {
    if !matches!(item_fn.vis, syn::Visibility::Public(_)) {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[route] attribute: function must be public. Add `pub` before `fn`.",
        ));
    }
    if item_fn.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[route] attribute: function must be async. Add `async` before `fn`.",
        ));
    }
    Ok(())
}

/// Process route attribute - extracted for testability
/// The `#[route]` expansion. A rejected attribute keeps the item next to its
/// error: `vespera!` still finds the handler in the route file and mounts it,
/// so dropping it only added a confusing E0425 at the `vespera!` call site.
pub fn route_attribute(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    process_route_attribute(attr, item.clone()).unwrap_or_else(|error| {
        let mut out = error.to_compile_error();
        out.extend(item);
        out
    })
}

pub fn process_route_attribute(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let route_args = syn::parse2::<args::RouteArgs>(attr)?;
    // The Path-extractor check, the alias expansion and `inline_module_of`
    // read source files through the file cache: a fresh epoch re-checks
    // their mtimes (rust-analyzer's long-lived proc-macro server).
    crate::schema_macro::file_cache::bump_epoch();
    let item_fn: RouteFnHead =
        syn::parse2(item.clone()).map_err(|error| route_item_error(&item, &error))?;
    validate_route_fn(&item_fn)?;
    let source_file = proc_macro2::Span::call_site().local_file();
    let inline_module = crate::cron_impl::inline_module_of(
        source_file.as_deref().and_then(std::path::Path::to_str),
        &syn::ext::IdentExt::unraw(&item_fn.sig.ident).to_string(),
        item_fn.sig.ident.span().start(),
    );
    path_check::check_path_extractors(
        &item_fn.sig,
        route_args.path.as_ref().map(syn::LitStr::value).as_deref(),
        source_file.as_deref(),
        inline_module.as_deref(),
    )?;
    let fn_sig = &stored_signature(&item_fn.sig, source_file.as_deref());

    // A generic handler's response types may not name its type parameters
    // (rejected in `typed_response_assertions`); its other types are asserted.
    // Checked before the route is stored, so a rejected entry never reaches
    // `ROUTE_STORAGE` (round-40 MR3-L3).
    let generics = generic_scope::param_names(&item_fn.sig.generics);
    let assertions = route_args
        .responses
        .as_ref()
        .map(|arr| typed_response_assertions(arr, &generics))
        .transpose()?
        .unwrap_or_default();

    // Store route metadata for later consumption by vespera!() macro
    let (summary, description) = crate::route::doc_summary_and_description(
        route_args.summary.as_ref().map(syn::LitStr::value),
        route_args.description.as_ref().map(syn::LitStr::value),
        &item_fn.attrs,
    );
    let stored = StoredRouteInfo {
        fn_name: item_fn.sig.ident.to_string(),
        method: route_args.method.as_ref().map(syn::Ident::to_string),
        custom_path: route_args.path.as_ref().map(syn::LitStr::value),
        success_status: route_args.success_status,
        error_status: match route_args.error_status.as_ref() {
            Some(arr) => extract_error_status_codes(arr)?,
            None => None,
        },
        typed_responses: match route_args.responses.as_ref() {
            Some(arr) => extract_typed_responses(arr)?,
            None => None,
        },
        tags: match route_args.tags.as_ref() {
            Some(arr) => extract_tag_strings(arr)?,
            None => None,
        },
        security: match route_args.security.as_ref() {
            Some(arr) => Some(crate::security::parse_security_requirements(
                arr,
                "#[route] `security`",
            )?),
            None => None,
        },
        headers: route_args.headers.unwrap_or_default(),
        operation_id: route_args.operation_id.as_ref().map(syn::LitStr::value),
        summary,
        request_example: route_args
            .request_example
            .as_ref()
            .map(parse_example_string),
        response_example: route_args
            .response_example
            .as_ref()
            .map(parse_example_string),
        deprecated: route_args.deprecated || crate::route::has_deprecated_attr(&item_fn.attrs),
        description,
        fn_sig_str: quote::quote!(#fn_sig).to_string(),
        inline_module,
        file_path: source_file.map(|p| p.display().to_string()),
    };
    register_route(stored);

    let mut output = item;
    output.extend(assertions);
    #[cfg(feature = "validation")]
    output.extend(validated_payload_assertions(&item_fn.sig));
    Ok(output)
}

/// `U: garde::Validate` assertions for the payload of every
/// `Validated<E<U>>` / `ValidatedWith<C, E<U>>` argument (`E` one of
/// `Json` / `Form` / `Query` / `Path` / `TypedMultipart`), spanned at `U`.
///
/// `#[derive(Schema)]` only generates `garde::Validate` for a named-field
/// struct with at least one runtime `#[schema(...)]` rule, so a rule-less
/// payload would otherwise surface as an opaque `Handler` trait-bound error at
/// the router. The bound goes through `vespera`'s own `AssertValidate`, whose
/// `#[diagnostic::on_unimplemented]` explains the fix at the argument.
///
/// A payload that names one of a generic handler's own parameters is skipped:
/// that type is out of scope for an item emitted next to the function. Any
/// other payload of such a handler is still asserted.
#[cfg(feature = "validation")]
fn validated_payload_assertions(sig: &syn::Signature) -> proc_macro2::TokenStream {
    let generics = generic_scope::param_names(&sig.generics);
    let assertions = sig
        .inputs
        .iter()
        .filter_map(|input| match input {
            syn::FnArg::Typed(pat_type) => crate::parser::validated_payload_type(&pat_type.ty),
            syn::FnArg::Receiver(_) => None,
        })
        .filter(|payload| !generic_scope::mentions(*payload, &generics))
        .map(|payload| {
            quote::quote_spanned! { syn::spanned::Spanned::span(payload) =>
                const _: () = {
                    const fn __vespera_assert_validate<T: ::vespera::__validation::AssertValidate + ?Sized>() {}
                    let _ = __vespera_assert_validate::<#payload>;
                };
            }
        });
    quote::quote! { #(#assertions)* }
}

/// `T: ::vespera::Schema` assertions for every type in
/// `responses = [(404, NotFound), ...]`, spanned at the type path.
///
/// The OpenAPI document references each type as a component schema; a type
/// without `#[derive(Schema)]` (or a typo) would otherwise emit a dangling
/// `$ref`. With the assertion it is a compile error pointing at the type.
///
/// An entry naming one of `generics` (a generic handler's own type or const
/// parameter) is a spanned error: the response type is only known per
/// instantiation, so no schema can be asserted or referenced beside the
/// handler (the spec would carry a dangling `$ref` to the parameter).
fn typed_response_assertions(
    arr: &syn::ExprArray,
    generics: &[String],
) -> syn::Result<proc_macro2::TokenStream> {
    let assertions = arr
        .elems
        .iter()
        .filter_map(|elem| match elem {
            syn::Expr::Tuple(tuple) => match tuple.elems.get(1) {
                Some(syn::Expr::Path(path)) => Some(&path.path),
                _ => None,
            },
            _ => None,
        })
        .map(|path| {
            if generic_scope::mentions(path, generics) {
                return Err(syn::Error::new_spanned(
                    path,
                    "a generic handler's type or const parameter cannot be a `responses` type: \
                     its schema is only known per instantiation, so the spec would reference \
                     the parameter as a dangling `$ref`",
                ));
            }
            Ok(quote::quote_spanned! { syn::spanned::Spanned::span(path) =>
                const _: () = {
                    const fn __vespera_assert_schema<T: ::vespera::Schema + ?Sized>() {}
                    let _ = __vespera_assert_schema::<#path>;
                };
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote::quote! { #(#assertions)* })
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use rstest::rstest;

    use super::*;

    // ========== Tests for validate_route_fn ==========

    #[test]
    fn test_validate_route_fn_not_public() {
        let item: RouteFnHead = syn::parse_quote! {
            async fn private_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be public")
        );
    }

    #[test]
    fn test_validate_route_fn_not_async() {
        let item: RouteFnHead = syn::parse_quote! {
            pub fn sync_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be async")
        );
    }

    #[test]
    fn test_validate_route_fn_valid() {
        let item: RouteFnHead = syn::parse_quote! {
            pub async fn valid_handler() -> String {
                "test".to_string()
            }
        };
        let result = validate_route_fn(&item);
        assert!(result.is_ok());
    }

    // ========== Tests for process_route_attribute ==========

    #[test]
    fn test_process_route_attribute_valid() {
        let attr = quote!(get);
        let item = quote!(
            pub async fn handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item.clone());
        assert!(result.is_ok());
        // Should return the original item unchanged
        assert_eq!(result.unwrap().to_string(), item.to_string());
    }

    #[test]
    fn typed_responses_emit_schema_assertions() {
        let attr = quote!(get, responses = [(404, NotFound), (409, errors::Conflict)]);
        let item = quote!(
            pub async fn handler() -> String {
                "ok".to_string()
            }
        );
        let output = process_route_attribute(attr, item).unwrap();
        let file: syn::File = syn::parse2(output).unwrap();
        insta::assert_snapshot!(
            "route_typed_response_schema_assertions",
            prettyplease::unparse(&file)
        );

        // Non-path entries (rejected by the argument parser) emit nothing.
        let array: syn::ExprArray = syn::parse_quote!([(404, 1), 2]);
        assert!(typed_response_assertions(&array, &[]).unwrap().is_empty());
    }

    #[test]
    fn test_process_route_attribute_invalid_attr() {
        let attr = quote!(invalid_method);
        let item = quote!(
            pub async fn handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_route_attribute_not_function() {
        let attr = quote!(get);
        let item = quote!(
            struct NotAFunction;
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("can only be applied to functions"));
    }

    /// The handler body is only delimited, never parsed: the item is
    /// re-emitted verbatim (so even tokens that are no valid block reach
    /// rustc, which reports them precisely), while a missing body or trailing
    /// tokens are rejected with syn's own message for the function.
    #[rstest]
    #[case::bodyless(quote!(pub async fn h();), false)]
    #[case::trailing_tokens(quote!(pub async fn h() {} extra), false)]
    #[case::unparsed_body(quote!(pub async fn h() { let = ; }), true)]
    #[case::attributed(quote!(#[doc = "x"] pub async fn h() -> u8 { 1 }), true)]
    fn route_body_is_delimited_but_not_parsed(
        #[case] item: proc_macro2::TokenStream,
        #[case] accepted: bool,
    ) {
        match process_route_attribute(quote!(get), item.clone()) {
            Ok(output) => {
                assert!(accepted, "must be rejected: {item}");
                assert_eq!(output.to_string(), item.to_string(), "re-emitted verbatim");
            }
            Err(err) => {
                assert!(!accepted, "must be accepted: {item}: {err}");
                assert!(err.to_string().starts_with("#[route] attribute: "), "{err}");
            }
        }
    }

    /// A function whose parameter list does not parse keeps syn's message: the
    /// user is told the fault, not sent to move the attribute.
    #[test]
    fn test_process_route_attribute_reports_a_malformed_signature() {
        let item = quote!(pub async fn list(Query(q) Query<Q>) {});
        let err = process_route_attribute(quote!(get), item)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("#[route] attribute: "), "{err}");
        assert!(!err.contains("not other items"), "{err}");
    }

    #[test]
    fn test_process_route_attribute_not_public() {
        let attr = quote!(get);
        let item = quote!(
            async fn private_handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be public"));
    }

    #[test]
    fn test_process_route_attribute_not_async() {
        let attr = quote!(get);
        let item = quote!(
            pub fn sync_handler() -> String {
                "ok".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be async"));
    }

    #[test]
    fn test_process_route_attribute_with_path() {
        let attr = quote!(get, path = "/users/{id}");
        let item = quote!(
            pub async fn get_user() -> String {
                "user".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_route_attribute_with_tags() {
        let attr = quote!(post, tags = ["users", "admin"]);
        let item = quote!(
            pub async fn create_user() -> String {
                "created".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_route_attribute_all_methods() {
        let methods = ["get", "post", "put", "patch", "delete", "head", "options"];
        for method in methods {
            let attr: proc_macro2::TokenStream = method.parse().unwrap();
            let item = quote!(
                pub async fn handler() -> String {
                    "ok".to_string()
                }
            );
            let result = process_route_attribute(attr, item);
            assert!(result.is_ok(), "Method {method} should be valid");
        }
    }

    // ========== Tests for ROUTE_STORAGE population ==========

    #[test]
    fn test_route_storage_populated_by_process_route_attribute() {
        let attr = quote!(
            get,
            path = "/{id}",
            tags = ["users"],
            description = "Get user by ID",
            error_status = [404]
        );
        let item = quote!(
            pub async fn get_user_test_storage() -> String {
                "test".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());

        // Find our entry by unique fn_name (the current crate's routes are
        // shared across parallel tests in this crate).
        let storage = current_crate_routes();

        // Find our entry and verify fields
        let stored = storage
            .iter()
            .find(|s| s.fn_name == "get_user_test_storage");
        assert!(
            stored.is_some(),
            "StoredRouteInfo should be in ROUTE_STORAGE"
        );
        let stored = stored.unwrap();
        assert_eq!(stored.method, Some("get".to_string()));
        assert_eq!(stored.custom_path, Some("/{id}".to_string()));
        assert_eq!(stored.tags, Some(vec!["users".to_string()]));
        assert_eq!(stored.description, Some("Get user by ID".to_string()));
        assert_eq!(stored.error_status, Some(vec![404]));
        assert_eq!(stored.headers.len(), 0);
        assert!(stored.fn_sig_str.contains("get_user_test_storage"));
    }

    #[test]
    fn test_route_storage_no_optional_fields() {
        let attr = quote!();
        let item = quote!(
            pub async fn minimal_handler_test() -> String {
                "test".to_string()
            }
        );
        let result = process_route_attribute(attr, item);
        assert!(result.is_ok());

        let storage = current_crate_routes();

        let stored = storage.iter().find(|s| s.fn_name == "minimal_handler_test");
        assert!(stored.is_some());
        let stored = stored.unwrap();
        assert_eq!(stored.method, None);
        assert_eq!(stored.custom_path, None);
        assert_eq!(stored.tags, None);
        assert_eq!(stored.description, None);
        assert_eq!(stored.error_status, None);
        assert_eq!(stored.headers.len(), 0);
    }

    #[test]
    fn test_register_route_replaces_same_file_and_function() {
        let file_path = Some("/tmp/vespera/routes/replaced.rs".to_string());
        let fn_name = "__test_replace_route".to_string();
        let base = StoredRouteInfo {
            fn_name: fn_name.clone(),
            method: Some("get".to_string()),
            custom_path: Some("/before".to_string()),
            success_status: None,
            error_status: None,
            typed_responses: None,
            tags: None,
            security: None,
            headers: Vec::new(),
            operation_id: None,
            summary: None,
            request_example: None,
            response_example: None,
            deprecated: false,
            description: None,
            file_path: file_path.clone(),
            fn_sig_str: "pub async fn __test_replace_route ()".to_string(),
            inline_module: None,
        };
        register_route(base);
        register_route(StoredRouteInfo {
            method: Some("post".to_string()),
            custom_path: Some("/after".to_string()),
            file_path,
            fn_sig_str: "pub async fn __test_replace_route ()".to_string(),
            inline_module: None,
            ..current_crate_routes()
                .iter()
                .find(|entry| entry.fn_name == fn_name)
                .cloned()
                .expect("first route registration should exist")
        });

        let matches: Vec<_> = current_crate_routes()
            .iter()
            .filter(|entry| entry.fn_name == fn_name)
            .cloned()
            .collect();
        assert_eq!(matches.len(), 1, "same source route should replace");
        assert_eq!(matches[0].method, Some("post".to_string()));
        assert_eq!(matches[0].custom_path, Some("/after".to_string()));
    }

    /// A top-level handler and a same-named one inside an inline module are
    /// two entries — the inline one must not overwrite (and so hide) the
    /// top-level registration, nor vice versa.
    #[test]
    fn test_register_route_keys_by_inline_module() {
        let file_path = Some("/tmp/vespera/routes/scoped.rs".to_string());
        let fn_name = "__test_scoped_route".to_string();
        for scope in ["", "v2"] {
            register_route(StoredRouteInfo {
                fn_name: fn_name.clone(),
                file_path: file_path.clone(),
                inline_module: Some(scope.to_string()),
                ..StoredRouteInfo::default()
            });
        }
        let scopes: Vec<_> = current_crate_routes()
            .iter()
            .filter(|entry| entry.fn_name == fn_name)
            .map(|entry| entry.inline_module.clone())
            .collect();
        assert_eq!(scopes, [Some(String::new()), Some("v2".to_string())]);
    }

    /// `#[route]` reads files through the file cache (Path-extractor check,
    /// `inline_module_of`), so every invocation starts a fresh epoch.
    #[test]
    fn test_route_attribute_bumps_the_file_cache_epoch() {
        let before = crate::schema_macro::file_cache::current_epoch();
        process_route_attribute(
            quote!(get),
            quote!(
                pub async fn epoch_probe() {}
            ),
        )
        .unwrap();
        assert_ne!(crate::schema_macro::file_cache::current_epoch(), before);
    }

    #[test]
    fn test_extract_tag_strings_empty() {
        let arr: syn::ExprArray = syn::parse_quote!([]);
        assert_eq!(extract_tag_strings(&arr).unwrap(), None);
    }

    #[test]
    fn test_extract_tag_strings_values() {
        let arr: syn::ExprArray = syn::parse_quote!(["users", "admin", "api"]);
        assert_eq!(
            extract_tag_strings(&arr).unwrap(),
            Some(vec![
                "users".to_string(),
                "admin".to_string(),
                "api".to_string()
            ])
        );
    }

    #[test]
    fn test_extract_tag_strings_rejects_non_string() {
        // `tags = [users]` (bare ident, missing quotes) must be a compile
        // error, not silently dropped.
        let arr: syn::ExprArray = syn::parse_quote!([users]);
        let err = extract_tag_strings(&arr).expect_err("non-string tag must error");
        assert!(err.to_string().contains("string literals"), "{err}");
    }

    #[rstest]
    #[case::json_array("[1,2]", serde_json::json!([1, 2]))]
    #[case::plain_text("not json", serde_json::Value::String("not json".to_string()))]
    fn example_parser_preserves_json_or_falls_back_to_text(
        #[case] source: &str,
        #[case] expected: serde_json::Value,
    ) {
        let literal = syn::LitStr::new(source, proc_macro2::Span::call_site());

        assert_eq!(parse_example_string(&literal), expected);
    }

    #[test]
    fn stored_signature_expands_return_alias_from_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("users.rs");
        std::fs::write(&file, "pub type ApiResult<T> = Result<Json<T>, AppError>;").unwrap();

        let aliased: syn::Signature = syn::parse_quote!(async fn get_user() -> ApiResult<User>);
        let stored = stored_signature(&aliased, Some(&file));
        assert_eq!(
            quote!(#stored).to_string(),
            "async fn get_user () -> Result < Json < User > , AppError >"
        );

        // Without a source file, or for a return type that cannot be an
        // alias, the signature is stored verbatim.
        let unchanged = stored_signature(&aliased, None);
        assert_eq!(quote!(#unchanged).to_string(), quote!(#aliased).to_string());
        let plain: syn::Signature = syn::parse_quote!(async fn list() -> Json<Vec<User>>);
        let stored_plain = stored_signature(&plain, Some(&file));
        assert_eq!(
            quote!(#stored_plain).to_string(),
            quote!(#plain).to_string()
        );
    }
}

#[cfg(all(test, feature = "validation"))]
mod tests_validated;

#[cfg(test)]
mod tests_extractors;

#[cfg(test)]
mod tests_item;

#[cfg(test)]
mod tests_responses;
#[cfg(test)]
mod tests_scope;
