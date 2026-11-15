//! Cron attribute macro implementation.
//!
//! This module implements the `#[vespera::cron]` attribute macro that validates
//! and processes functions for cron job registration.
//!
//! # Overview
//!
//! The `#[cron]` attribute is applied to functions to:
//! - Validate that the function is `pub async fn`
//! - Validate that the function takes no parameters
//! - Parse the cron expression string and the optional `tz` / `overlap`
//!   arguments (a `tz` name is checked against the IANA database under the
//!   `cron-tz` feature, and rejected without it)
//! - Mark the function for cron discovery by the `vespera!` macro
//!
//! # Example
//!
//! ```ignore
//! #[vespera::cron("0 */5 * * * *")]
//! pub async fn cleanup_sessions() {
//!     println!("Running cleanup");
//! }
//!
//! #[vespera::cron("0 0 9 * * *", tz = "Asia/Seoul", overlap = "skip")]
//! pub async fn morning_report() {}
//! ```

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::macro_storage::{CrateStorage, SourceIdentified};
use crate::metadata::CronOptions;

/// Metadata stored by `#[cron]` for later consumption by `vespera!()`.
///
/// Each invocation of `#[cron]` pushes one entry into [`CRON_STORAGE`].
/// The `vespera!()` macro reads this storage to build the cron scheduler.
#[derive(Debug, Clone)]
pub struct StoredCronInfo {
    /// Function name (e.g., `"cleanup_sessions"`)
    pub fn_name: String,
    /// Cron expression (e.g., `"0 */5 * * * *"`)
    pub expression: String,
    /// Source file path from `Span::call_site().local_file()`
    pub file_path: Option<String>,
    /// Inline `mod` path inside `file_path` that encloses the function
    /// (`""` = file level, `"a::b"` = `mod a { mod b { … } }`), or `None`
    /// when it could not be located (no file, macro-generated function, an
    /// IDE buffer that differs from the file on disk).  Part of the storage
    /// key, so same-named jobs in different inline modules of one file do
    /// not replace each other.
    pub inline_module: Option<String>,
    /// Optional `tz` / `overlap` arguments
    pub options: CronOptions,
}

/// Parsed `#[cron("expr", tz = "Area/City", overlap = "allow" | "skip")]`
/// arguments.  Only the expression literal is required; `tz` and `overlap`
/// may appear once each, in any order.
pub struct CronArgs {
    /// The cron expression literal
    pub expression: syn::LitStr,
    /// The `tz = "..."` literal, if any
    pub timezone: Option<syn::LitStr>,
    /// `overlap = "skip"`
    pub skip_overlap: bool,
}

impl CronArgs {
    /// The options to store for `vespera!` code generation.
    pub fn options(&self) -> CronOptions {
        CronOptions {
            timezone: self.timezone.as_ref().map(syn::LitStr::value),
            skip_overlap: self.skip_overlap,
        }
    }
}

impl syn::parse::Parse for CronArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let expression: syn::LitStr = input.parse().map_err(|_| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[cron] attribute: expected a cron expression string. Example: #[cron(\"0 */5 * * * *\")]",
            )
        })?;
        let mut args = Self {
            expression,
            timezone: None,
            skip_overlap: false,
        };
        let mut overlap_seen = false;
        while !input.is_empty() {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: syn::LitStr = input.parse()?;
            match key.to_string().as_str() {
                "tz" if args.timezone.is_none() => args.timezone = Some(value),
                "overlap" if !overlap_seen => {
                    overlap_seen = true;
                    args.skip_overlap = parse_overlap(&value)?;
                }
                "tz" | "overlap" => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!("#[cron] attribute: duplicate `{key}` argument."),
                    ));
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "#[cron] attribute: unknown argument `{key}`. Expected `tz = \"Area/City\"` or `overlap = \"allow\" | \"skip\"`."
                        ),
                    ));
                }
            }
        }
        Ok(args)
    }
}

/// `"skip"` → `true`, `"allow"` → `false`, anything else is an error.
fn parse_overlap(value: &syn::LitStr) -> syn::Result<bool> {
    match value.value().as_str() {
        "skip" => Ok(true),
        "allow" => Ok(false),
        other => Err(syn::Error::new_spanned(
            value,
            format!(
                "#[cron] attribute: unknown overlap policy `{other}`. Expected \"allow\" (default) or \"skip\"."
            ),
        )),
    }
}

/// Per-crate storage for cron metadata collected by `#[cron]` attribute
/// macros, read by `vespera!()` to build the cron scheduler.
///
/// Keyed by [`crate::schema_impl::current_crate_key`] so a long-lived
/// rust-analyzer proc-macro server (one process, many crates) never schedules
/// crate A's cron jobs into crate B. See
/// [`SCHEMA_STORAGE`](crate::schema_impl::SCHEMA_STORAGE) for the rationale.
pub static CRON_STORAGE: CrateStorage<StoredCronInfo> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl SourceIdentified for StoredCronInfo {
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

/// The inline-module path (see [`StoredCronInfo::inline_module`]) enclosing
/// the function `fn_name` whose name token starts on `line` of `file`.
///
/// The file is re-parsed with proc-macro2's fallback implementation forced:
/// tokens parsed from a string through the compiler bridge all carry the
/// call-site span, so only the fallback reports real line numbers.  Only the
/// returned `String` leaves this function, so no fallback token is ever mixed
/// with compiler tokens.
pub fn inline_module_of(file: Option<&str>, fn_name: &str, line: usize) -> Option<String> {
    // Declarations compare unraw: `r#match` (a route's stored name) is the
    // `match` the AST walk sees.
    let fn_name = fn_name.strip_prefix("r#").unwrap_or(fn_name);
    let content = crate::schema_macro::file_cache::get_file_content(std::path::Path::new(file?))?;
    // No `mod` anywhere: every item is top-level — skip the re-parse that
    // `#[route]` would otherwise pay once per handler.
    if !content.contains("mod") {
        return Some(String::new());
    }
    let ast = parse_with_lines(&content)?;
    find_fn_module(&ast.items, fn_name, line, "")
}

/// Parse `content` with proc-macro2's fallback forced, so spans carry real
/// line numbers (see [`inline_module_of`]).
fn parse_with_lines(content: &str) -> Option<syn::File> {
    /// Restores proc-macro2's normal mode even if parsing panics.
    struct Unforce;
    impl Drop for Unforce {
        fn drop(&mut self) {
            proc_macro2::fallback::unforce();
        }
    }
    proc_macro2::fallback::force();
    let _unforce = Unforce;
    syn::parse_file(content).ok()
}

/// Reject a cron function `vespera!` could not call as
/// `crate::module::function`: one declared (on `line` of `file`) inside an
/// `impl` / `trait` block or another function's body.  Without this the job
/// fell back to the file's module path and either failed to resolve at the
/// `vespera!` call site or — worse — scheduled a same-named free function.
pub fn check_free_function(
    file: Option<&str>,
    item_fn: &syn::ItemFn,
    line: usize,
) -> syn::Result<()> {
    let fn_name = syn::ext::IdentExt::unraw(&item_fn.sig.ident).to_string();
    let nested = file
        .and_then(|file| {
            crate::schema_macro::file_cache::get_file_content(std::path::Path::new(file))
        })
        .and_then(|content| parse_with_lines(&content))
        .is_some_and(|ast| items_nest_fn(&mut ast.items.iter(), &fn_name, line));
    if nested {
        return Err(syn::Error::new_spanned(
            &item_fn.sig.ident,
            "#[cron] attribute: a cron job must be a free function of a module, not an associated function or a function inside another function's body: `vespera!` calls it as `crate::module::function`.",
        ));
    }
    Ok(())
}

/// Whether `items` declare the function `fn_name` on `line` anywhere other
/// than at module level: in an `impl` / `trait` block, a function body (also
/// inside nested blocks, closures and `async` blocks) or a `const` / `static`
/// initialiser (`const _: () = { .. };`).
fn items_nest_fn(items: &mut dyn Iterator<Item = &syn::Item>, fn_name: &str, line: usize) -> bool {
    use syn::visit::Visit;

    /// Walks every item and expression; `depth` counts the enclosing scopes
    /// that are not modules.
    struct Finder<'a> {
        fn_name: &'a str,
        line: usize,
        depth: usize,
        found: bool,
    }
    impl Finder<'_> {
        fn is_target(&self, sig: &syn::Signature) -> bool {
            syn::ext::IdentExt::unraw(&sig.ident) == self.fn_name
                && sig.ident.span().start().line == self.line
        }
        fn nested(&mut self, visit: impl FnOnce(&mut Self)) {
            self.depth += 1;
            visit(self);
            self.depth -= 1;
        }
    }
    impl<'ast> Visit<'ast> for Finder<'_> {
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            self.found |= self.depth > 0 && self.is_target(&function.sig);
            self.nested(|this| syn::visit::visit_item_fn(this, function));
        }
        fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
            self.found |= self.is_target(&function.sig);
            self.nested(|this| syn::visit::visit_impl_item_fn(this, function));
        }
        fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
            self.found |= self.is_target(&function.sig);
            self.nested(|this| syn::visit::visit_trait_item_fn(this, function));
        }
        fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
            self.nested(|this| syn::visit::visit_item_const(this, item));
        }
        fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
            self.nested(|this| syn::visit::visit_item_static(this, item));
        }
    }

    let mut finder = Finder {
        fn_name,
        line,
        depth: 0,
        found: false,
    };
    for item in items {
        finder.visit_item(item);
    }
    finder.found
}

/// Depth-first search of `items` (and the inline modules inside them) for the
/// function `fn_name` declared on `line`; returns the inline path to it.
fn find_fn_module(items: &[syn::Item], fn_name: &str, line: usize, prefix: &str) -> Option<String> {
    items.iter().find_map(|item| match item {
        syn::Item::Fn(function)
            if syn::ext::IdentExt::unraw(&function.sig.ident) == fn_name
                && function.sig.ident.span().start().line == line =>
        {
            Some(prefix.to_owned())
        }
        syn::Item::Mod(syn::ItemMod {
            ident,
            content: Some((_, inner)),
            ..
        }) => {
            let name = syn::ext::IdentExt::unraw(ident).to_string();
            let child = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}::{name}")
            };
            find_fn_module(inner, fn_name, line, &child)
        }
        _ => None,
    })
}

/// Replace-insert a `#[cron]` metadata entry in the current crate's bucket.
pub fn register_cron(info: StoredCronInfo) {
    crate::macro_storage::register(&CRON_STORAGE, info);
}

/// Snapshot of the current crate's registered cron jobs — a cheap `Arc` clone.
#[must_use]
pub fn current_crate_crons() -> Arc<Vec<StoredCronInfo>> {
    crate::macro_storage::current_crate_items(&CRON_STORAGE)
}

/// Validate cron function - must be pub, async, take no parameters and
/// return `()`.
pub fn validate_cron_fn(item_fn: &syn::ItemFn) -> Result<(), syn::Error> {
    if !matches!(item_fn.vis, syn::Visibility::Public(_)) {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[cron] attribute: function must be public. Add `pub` before `fn`.",
        ));
    }
    if item_fn.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            item_fn.sig.fn_token,
            "#[cron] attribute: function must be async. Add `async` before `fn`.",
        ));
    }
    if !item_fn.sig.inputs.is_empty() {
        return Err(syn::Error::new_spanned(
            &item_fn.sig.inputs,
            "#[cron] attribute: cron functions must take no parameters.",
        ));
    }
    if !item_fn.sig.generics.params.is_empty() || item_fn.sig.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &item_fn.sig.generics,
            "#[cron] attribute: cron functions must not be generic: the scheduler calls them without type arguments.",
        ));
    }
    let returns_unit = match &item_fn.sig.output {
        syn::ReturnType::Default => true,
        syn::ReturnType::Type(_, ty) => {
            matches!(&**ty, syn::Type::Tuple(tuple) if tuple.elems.is_empty())
        }
    };
    if !returns_unit {
        return Err(syn::Error::new_spanned(
            &item_fn.sig.output,
            "#[cron] attribute: cron functions must return `()`: the scheduler awaits the job and discards nothing. Handle (or log) errors inside the job.",
        ));
    }
    if let syn::Safety::Unsafe(unsafety) = &item_fn.sig.safety {
        return Err(syn::Error::new_spanned(
            unsafety,
            "#[cron] attribute: cron functions must not be `unsafe`: the scheduler calls them from safe code.",
        ));
    }
    Ok(())
}

/// Message of the `#[vespera::cron]` stand-in used without vespera's `cron`
/// feature.
pub const FEATURE_DISABLED_MSG: &str = "`#[vespera::cron]` requires vespera's `cron` feature: \
     enable it with vespera = { features = [\"cron\"] }";

/// Expansion of the `#[vespera::cron]` stand-in (vespera's `cron` feature
/// off): the item unchanged plus a compile error naming the feature, spanned
/// on the function name (the whole item when it is not a function). No job
/// is registered, so `vespera!` never references `vespera::cron`.
pub fn feature_disabled_error(item: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    let error = match syn::parse2::<syn::ItemFn>(item.clone()) {
        Ok(item_fn) => syn::Error::new(item_fn.sig.ident.span(), FEATURE_DISABLED_MSG),
        Err(_) => syn::Error::new_spanned(&item, FEATURE_DISABLED_MSG),
    };
    let mut out = error.to_compile_error();
    out.extend(item);
    out
}

/// Process cron attribute - extracted for testability
pub fn process_cron_attribute(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let args: CronArgs = syn::parse2(attr)?;
    // `inline_module_of` reads the source file through the file cache; a
    // fresh epoch re-checks its mtime (rust-analyzer's long-lived server).
    crate::schema_macro::file_cache::bump_epoch();
    // Compile-time cron-syntax validation (gated by the `cron` feature, enabled
    // transitively by `vespera`'s `cron` feature). A malformed expression is a
    // span-attached compile error here instead of a failure at app startup.
    #[cfg(feature = "cron")]
    validate_cron_expression(&args.expression)?;
    if let Some(timezone) = &args.timezone {
        validate_timezone(timezone)?;
    }
    let item_fn: syn::ItemFn = syn::parse2(item.clone()).map_err(|e| syn::Error::new(e.span(), "#[cron] attribute: can only be applied to functions, not other items. Move or remove the attribute."))?;
    validate_cron_fn(&item_fn)?;

    // Unraw: the module-tree scan and the stored-job dedup key on
    // `r#match` as `match` (the generated call path re-adds the `r#`).
    let fn_name = syn::ext::IdentExt::unraw(&item_fn.sig.ident).to_string();
    let file_path = proc_macro2::Span::call_site()
        .local_file()
        .map(|p| p.display().to_string());
    let line = item_fn.sig.ident.span().start().line;
    check_free_function(file_path.as_deref(), &item_fn, line)?;
    let stored = StoredCronInfo {
        inline_module: inline_module_of(file_path.as_deref(), &fn_name, line),
        fn_name,
        expression: args.expression.value(),
        file_path,
        options: args.options(),
    };
    register_cron(stored);

    let mut output = item;
    output.extend(send_assertion(&item_fn.sig.ident));
    Ok(output)
}

/// A never-called check that the job's future is `Send`, spanned at the job:
/// the scheduler boxes it as `Pin<Box<dyn Future<Output = ()> + Send>>`, and
/// a non-`Send` future (an `Rc` held across an `.await`) otherwise failed far
/// away, inside the `vespera!` expansion.
fn send_assertion(ident: &syn::Ident) -> proc_macro2::TokenStream {
    quote::quote_spanned! {ident.span()=>
        const _: () = {
            fn __vespera_cron_job_must_be_send<T: ::core::marker::Send>(_: T) {}
            #[allow(dead_code)]
            fn __vespera_cron_send_check() { __vespera_cron_job_must_be_send(#ident()); }
        };
    }
}

/// Validate a cron expression at **compile time** with the exact parser the
/// runtime scheduler uses (`vespera_core::cron`), so a malformed expression is
/// a clean span-attached compile error instead of a startup failure.
#[cfg(feature = "cron")]
fn validate_cron_expression(expression: &syn::LitStr) -> syn::Result<()> {
    let expr = expression.value();
    vespera_core::cron::parse(&expr).map(drop).map_err(|e| {
        syn::Error::new_spanned(
            expression,
            format!(
                "#[cron] invalid cron expression `{expr}`: {e}. Expected a 6-field \
                 expression `sec min hour day month weekday`, e.g. \"0 */5 * * * *\"."
            ),
        )
    })
}

/// Check a `tz = "..."` name at **compile time** against the IANA database the
/// runtime scheduler resolves it with (`vespera_core::cron::parse_timezone`).
#[cfg(feature = "cron-tz")]
fn validate_timezone(timezone: &syn::LitStr) -> syn::Result<()> {
    let name = timezone.value();
    vespera_core::cron::parse_timezone(&name)
        .map(drop)
        .map_err(|_| {
            syn::Error::new_spanned(
                timezone,
                format!(
                    "#[cron] unknown time zone `{name}`. Expected an IANA name such as \
                 \"Asia/Seoul\", \"America/New_York\" or \"UTC\" (case-sensitive)."
                ),
            )
        })
}

/// Without the `cron-tz` feature there is no time-zone database to evaluate
/// `tz` with, so the argument is rejected instead of silently running in UTC.
#[cfg(not(feature = "cron-tz"))]
fn validate_timezone(timezone: &syn::LitStr) -> syn::Result<()> {
    Err(syn::Error::new_spanned(
        timezone,
        "#[cron] `tz = \"...\"` requires the `cron-tz` feature: \
         vespera = { features = [\"cron\", \"cron-tz\"] }. Without `tz`, jobs run in UTC.",
    ))
}

#[cfg(test)]
mod tests;
