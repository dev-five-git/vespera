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
    /// Restores proc-macro2's normal mode even if parsing panics.
    struct Unforce;
    impl Drop for Unforce {
        fn drop(&mut self) {
            proc_macro2::fallback::unforce();
        }
    }
    let content = crate::schema_macro::file_cache::get_file_content(std::path::Path::new(file?))?;
    // No `mod` anywhere: every item is top-level — skip the re-parse that
    // `#[route]` would otherwise pay once per handler.
    if !content.contains("mod") {
        return Some(String::new());
    }
    proc_macro2::fallback::force();
    let _unforce = Unforce;
    let ast = syn::parse_file(&content).ok()?;
    find_fn_module(&ast.items, fn_name, line, "")
}

/// Depth-first search of `items` (and the inline modules inside them) for the
/// function `fn_name` declared on `line`; returns the inline path to it.
fn find_fn_module(items: &[syn::Item], fn_name: &str, line: usize, prefix: &str) -> Option<String> {
    items.iter().find_map(|item| match item {
        syn::Item::Fn(function)
            if function.sig.ident == fn_name && function.sig.ident.span().start().line == line =>
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

/// Validate cron function - must be pub, async, and take no parameters.
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

    let fn_name = item_fn.sig.ident.to_string();
    let file_path = proc_macro2::Span::call_site()
        .local_file()
        .map(|p| p.display().to_string());
    let line = item_fn.sig.ident.span().start().line;
    let stored = StoredCronInfo {
        inline_module: inline_module_of(file_path.as_deref(), &fn_name, line),
        fn_name,
        expression: args.expression.value(),
        file_path,
        options: args.options(),
    };
    register_cron(stored);

    Ok(item)
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
mod tests {
    use quote::quote;

    use super::*;

    #[test]
    fn feature_disabled_stand_in_keeps_the_item_and_names_the_feature() {
        let item = quote!(
            pub async fn job() {}
        );
        let out = feature_disabled_error(item.clone()).to_string();
        assert!(out.starts_with(":: core :: compile_error !"), "{out}");
        assert!(
            out.contains(r#"vespera = { features = [\"cron\"] }"#),
            "{out}"
        );
        assert!(out.ends_with(&item.to_string()), "{out}");
        // Not a function: still an error (spanned on the item), item kept.
        let out = feature_disabled_error(quote!(
            struct S;
        ))
        .to_string();
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.ends_with("struct S ;"), "{out}");
    }

    // ========== Tests for validate_cron_fn ==========

    #[test]
    fn test_validate_cron_fn_not_public() {
        let item: syn::ItemFn = syn::parse_quote! {
            async fn private_job() {
                println!("job");
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be public")
        );
    }

    #[test]
    fn test_validate_cron_fn_not_async() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub fn sync_job() {
                println!("job");
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("function must be async")
        );
    }

    #[test]
    fn test_validate_cron_fn_has_params() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub async fn job_with_params(x: i32) {
                println!("{}", x);
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must take no parameters")
        );
    }

    #[test]
    fn test_validate_cron_fn_valid() {
        let item: syn::ItemFn = syn::parse_quote! {
            pub async fn valid_job() {
                println!("job");
            }
        };
        let result = validate_cron_fn(&item);
        assert!(result.is_ok());
    }

    // ========== Tests for process_cron_attribute ==========

    #[test]
    fn test_process_cron_attribute_valid() {
        let attr = quote!("0 */5 * * * *");
        let item = quote!(
            pub async fn my_job() {
                println!("running");
            }
        );
        let result = process_cron_attribute(attr, item.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap().to_string(), item.to_string());
    }

    #[test]
    fn test_process_cron_attribute_invalid_expression() {
        let attr = quote!(123);
        let item = quote!(
            pub async fn my_job() {
                println!("running");
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expected a cron expression string"));
    }

    #[test]
    fn test_process_cron_attribute_not_function() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            struct NotAFunction;
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("can only be applied to functions"));
    }

    #[test]
    fn test_process_cron_attribute_not_public() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            async fn private_job() {
                println!("job");
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be public"));
    }

    #[test]
    fn test_process_cron_attribute_not_async() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            pub fn sync_job() {
                println!("job");
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("function must be async"));
    }

    #[test]
    fn test_process_cron_attribute_with_params() {
        let attr = quote!("0 * * * * *");
        let item = quote!(
            pub async fn job(x: i32) {
                println!("{}", x);
            }
        );
        let result = process_cron_attribute(attr, item);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must take no parameters"));
    }

    #[test]
    fn test_register_cron_replaces_same_file_and_function() {
        let file_path = Some("/tmp/vespera/tasks/replaced.rs".to_string());
        let fn_name = "__test_replace_cron".to_string();
        register_cron(StoredCronInfo {
            fn_name: fn_name.clone(),
            expression: "0 */5 * * * *".to_string(),
            file_path: file_path.clone(),
            inline_module: None,
            options: CronOptions::default(),
        });
        register_cron(StoredCronInfo {
            fn_name: fn_name.clone(),
            expression: "0 */10 * * * *".to_string(),
            file_path,
            inline_module: None,
            options: CronOptions::default(),
        });

        let matches: Vec<_> = current_crate_crons()
            .iter()
            .filter(|entry| entry.fn_name == fn_name)
            .cloned()
            .collect();
        assert_eq!(matches.len(), 1, "same source cron should replace");
        assert_eq!(matches[0].expression, "0 */10 * * * *");
    }

    /// Same file + function name but different inline modules are two jobs;
    /// an unknown inline module still replaces (IDE re-expansion).
    #[test]
    fn test_register_cron_keys_by_inline_module() {
        let file_path = Some("/tmp/vespera/tasks/clash.rs".to_string());
        let fn_name = "__test_clash_cron".to_string();
        let entry = |inline: Option<&str>, expression: &str| StoredCronInfo {
            fn_name: fn_name.clone(),
            expression: expression.to_string(),
            file_path: file_path.clone(),
            inline_module: inline.map(str::to_string),
            options: CronOptions::default(),
        };
        let expressions = || -> Vec<(Option<String>, String)> {
            current_crate_crons()
                .iter()
                .filter(|entry| entry.fn_name == fn_name)
                .map(|entry| (entry.inline_module.clone(), entry.expression.clone()))
                .collect()
        };
        register_cron(entry(Some("a"), "1 * * * * *"));
        register_cron(entry(Some("b"), "2 * * * * *"));
        register_cron(entry(Some("a"), "3 * * * * *"));
        assert_eq!(
            expressions(),
            vec![
                (Some("a".to_string()), "3 * * * * *".to_string()),
                (Some("b".to_string()), "2 * * * * *".to_string()),
            ]
        );
        register_cron(entry(None, "4 * * * * *"));
        assert_eq!(expressions().len(), 2, "unknown scope replaces, never adds");
    }

    #[rstest::rstest]
    #[case::file_level("tick", 2, Some(""))]
    #[case::inline_a("tick", 5, Some("a"))]
    #[case::nested_raw_b("tick", 9, Some("b::inner"))]
    #[case::wrong_line("tick", 3, None)]
    #[case::unknown_fn("tock", 5, None)]
    fn test_inline_module_of_locates_the_declaration(
        #[case] fn_name: &str,
        #[case] line: usize,
        #[case] expected: Option<&str>,
    ) {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("jobs.rs");
        std::fs::write(
            &file,
            "\n#[cron(\"0 * * * * *\")] pub async fn tick() {}\nmod a {\n    pub fn other() {}\n    #[cron(\"1 * * * * *\")] pub async fn tick() {}\n}\nmod r#b {\n    mod inner {\n        #[cron(\"2 * * * * *\")] pub async fn tick() {}\n    }\n    mod external;\n}\n",
        )
        .unwrap();
        let file = file.display().to_string();
        assert_eq!(
            inline_module_of(Some(&file), fn_name, line),
            expected.map(str::to_string)
        );
    }

    #[rstest::rstest]
    #[case::no_file(None)]
    #[case::missing_file(Some("/definitely/not/here/jobs.rs"))]
    fn test_inline_module_of_without_a_readable_file(#[case] file: Option<&str>) {
        assert_eq!(inline_module_of(file, "tick", 1), None);
    }

    #[test]
    fn test_inline_module_of_unparsable_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("broken.rs");
        std::fs::write(&file, "mod {{{").unwrap();
        assert_eq!(
            inline_module_of(Some(&file.display().to_string()), "tick", 1),
            None
        );
    }

    // ===== Compile-time cron-syntax validation (gated by the `cron` feature) =====

    #[cfg(feature = "cron")]
    #[test]
    fn test_process_cron_attribute_valid_cron_syntax_passes() {
        for expr in [
            quote!("0 */5 * * * *"),
            quote!("1/10 * * * * *"),
            quote!("0 0 0 * * *"),
            quote!("0 30 9 * * Mon-Fri"),
        ] {
            let item = quote!(
                pub async fn my_job() {}
            );
            assert!(
                process_cron_attribute(expr.clone(), item).is_ok(),
                "expected valid cron `{expr}` to pass"
            );
        }
    }

    #[cfg(feature = "cron")]
    #[test]
    fn test_process_cron_attribute_invalid_cron_syntax_is_compile_error() {
        // Each is rejected at compile time (was a runtime scheduler failure):
        // 1-field, 5-field (missing seconds), out-of-range minute, garbage token.
        for bad in [
            quote!("invalid"),
            quote!("* * * * *"),
            quote!("0 99 * * * *"),
            quote!("not a cron at all"),
        ] {
            let item = quote!(
                pub async fn my_job() {}
            );
            let result = process_cron_attribute(bad.clone(), item);
            assert!(result.is_err(), "expected invalid cron `{bad}` to error");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("invalid cron expression"),
                "expected `invalid cron expression` message for `{bad}`"
            );
        }
    }

    // ===== `tz` / `overlap` arguments =====

    /// The optional arguments parse in any order, with a trailing comma.
    #[rstest::rstest]
    #[case::expression_only(quote!("0 * * * * *"), CronOptions::default())]
    #[case::trailing_comma(quote!("0 * * * * *",), CronOptions::default())]
    #[case::overlap_allow(quote!("0 * * * * *", overlap = "allow"), CronOptions::default())]
    #[case::overlap_skip(
        quote!("0 * * * * *", overlap = "skip"),
        CronOptions { timezone: None, skip_overlap: true }
    )]
    #[case::tz_and_overlap(
        quote!("0 * * * * *", overlap = "skip", tz = "Asia/Seoul",),
        CronOptions { timezone: Some("Asia/Seoul".to_owned()), skip_overlap: true }
    )]
    fn cron_args_parse_optional_arguments(
        #[case] attr: proc_macro2::TokenStream,
        #[case] expected: CronOptions,
    ) {
        let args: CronArgs = syn::parse2(attr).expect("valid arguments");
        assert_eq!(args.expression.value(), "0 * * * * *");
        assert_eq!(args.options(), expected);
    }

    /// Malformed optional arguments are span-attached compile errors.
    #[rstest::rstest]
    #[case::unknown_key(quote!("0 * * * * *", zone = "UTC"), "unknown argument `zone`")]
    #[case::duplicate_tz(quote!("0 * * * * *", tz = "UTC", tz = "UTC"), "duplicate `tz` argument")]
    #[case::duplicate_overlap(
        quote!("0 * * * * *", overlap = "skip", overlap = "skip"),
        "duplicate `overlap` argument"
    )]
    #[case::unknown_overlap(quote!("0 * * * * *", overlap = "queue"), "unknown overlap policy `queue`")]
    #[case::missing_comma(quote!("0 * * * * *" tz = "UTC"), "expected `,`")]
    #[case::missing_equals(quote!("0 * * * * *", tz "UTC"), "expected `=`")]
    #[case::non_literal_value(quote!("0 * * * * *", tz = UTC), "expected string literal")]
    fn cron_args_reject_malformed_arguments(
        #[case] attr: proc_macro2::TokenStream,
        #[case] message: &str,
    ) {
        let error = syn::parse2::<CronArgs>(attr)
            .err()
            .expect("malformed arguments must fail")
            .to_string();
        assert!(error.contains(message), "{error}");
    }

    /// `process_cron_attribute` stores the parsed options for `vespera!`.
    #[test]
    fn process_cron_attribute_stores_overlap_policy() {
        let item = quote!(
            pub async fn __test_overlap_skip_job() {}
        );
        process_cron_attribute(quote!("0 * * * * *", overlap = "skip"), item).expect("valid");
        let stored = current_crate_crons()
            .iter()
            .find(|entry| entry.fn_name == "__test_overlap_skip_job")
            .cloned()
            .expect("registered");
        assert!(stored.options.skip_overlap);
        assert_eq!(stored.options.timezone, None);
    }

    #[cfg(feature = "cron-tz")]
    #[rstest::rstest]
    #[case::known("Asia/Seoul", None)]
    #[case::utc("UTC", None)]
    #[case::wrong_case("asia/seoul", Some("unknown time zone `asia/seoul`"))]
    #[case::unknown("Mars/Olympus_Mons", Some("unknown time zone `Mars/Olympus_Mons`"))]
    fn process_cron_attribute_validates_time_zones(
        #[case] timezone: &str,
        #[case] error: Option<&str>,
    ) {
        let item = quote!(
            pub async fn my_job() {}
        );
        let result = process_cron_attribute(quote!("0 0 9 * * *", tz = #timezone), item);
        match error {
            None => assert!(result.is_ok(), "{timezone}"),
            Some(message) => {
                let error = result.expect_err("unknown zone must fail").to_string();
                assert!(error.contains(message), "{error}");
            }
        }
    }

    #[cfg(not(feature = "cron-tz"))]
    #[test]
    fn tz_without_cron_tz_feature_is_a_compile_error() {
        let item = quote!(
            pub async fn my_job() {}
        );
        let error = process_cron_attribute(quote!("0 0 9 * * *", tz = "Asia/Seoul"), item)
            .expect_err("tz needs the cron-tz feature")
            .to_string();
        assert!(error.contains("requires the `cron-tz` feature"), "{error}");
    }
}
