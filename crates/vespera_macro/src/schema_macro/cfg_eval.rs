//! Which `#[cfg(..)]`-gated items a source lookup can rule out.
//!
//! A file can hold cfg twins of one struct (`#[cfg(feature = "pg")] struct
//! Model { id: i64 }` / `#[cfg(not(feature = "pg"))] struct Model { id: i32
//! }`) or a dead fixture (`#[cfg(any())]`); the lookup must not derive from
//! the twin the compiler drops.  A predicate is evaluated three-valued: the
//! constants `all()` / `any()` always, `feature = ".."` against the
//! compiler's `--cfg feature=".."` arguments when the macro runs inside
//! rustc, everything else (`unix`, `test`, custom cfgs) is unknown and keeps
//! the item.

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;

use super::field_attrs::field_cfgs;

/// One cfg predicate: a `true` / `false` literal (stable since Rust 1.88,
/// which syn cannot parse as a `Meta`) or a `Meta` (`feature = ".."`,
/// `all(..)`, `unix`, ...).
#[derive(Clone)]
pub enum Predicate {
    Bool(syn::LitBool),
    Meta(Box<syn::Meta>),
}

impl Parse for Predicate {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(syn::LitBool) {
            return input.parse().map(Self::Bool);
        }
        input.parse().map(|meta| Self::Meta(Box::new(meta)))
    }
}

impl ToTokens for Predicate {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            Self::Bool(value) => value.to_tokens(tokens),
            Self::Meta(meta) => meta.to_tokens(tokens),
        }
    }
}

/// Whether one of `attrs`' `#[cfg(..)]` predicates is known to be false.
///
/// Called per item of every scanned module, so the (process-constant)
/// feature set is read only when an attribute is a `cfg` at all, and then
/// from a memo instead of re-scanning the compiler's argv.
pub fn statically_inactive(attrs: &[syn::Attribute]) -> bool {
    let mut features = None;
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr.parse_args::<Predicate>().is_ok_and(|predicate| {
                let features = features.get_or_insert_with(memoized_features);
                eval(&predicate, features.as_ref().as_deref()) == Some(false)
            })
    })
}

/// Whether the compiler drops the item or field carrying `attrs`: a statically
/// false `#[cfg]`, or a `cfg_attr(p, cfg(q))` whose `cfg(q)` is statically
/// false (rustc keeps it under `cfg(any(not(p), q))`, see
/// [`field_cfgs`]). Every per-item / per-field "which twin is live" check uses
/// this, so a twin disabled through `cfg_attr` is never the one read.
pub fn compiled_out(attrs: &[syn::Attribute]) -> bool {
    statically_inactive(&field_cfgs(attrs))
}

/// Whether `predicate` is known to hold (`all()`, `true`, an enabled
/// `feature = ".."` inside rustc); unknown counts as not active.
pub fn statically_active(predicate: &Predicate) -> bool {
    eval(predicate, memoized_features().as_ref().as_deref()) == Some(true)
}

/// Whether `predicate` is known to be false (`any()`, `false`, a disabled
/// `feature = ".."` inside rustc); unknown counts as not false. The mirror of
/// [`statically_active`] is this same test (R33-SM0: one function, not two).
pub fn statically_false(predicate: &Predicate) -> bool {
    eval(predicate, memoized_features().as_ref().as_deref()) == Some(false)
}

/// [`enabled_features`], computed once per process (the compiler's command
/// line cannot change); a per-thread test override of the arguments is read
/// afresh.
fn memoized_features() -> std::borrow::Cow<'static, Option<Vec<String>>> {
    static FEATURES: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
    if crate::env_access::compiler_args_overridden() {
        return std::borrow::Cow::Owned(enabled_features());
    }
    std::borrow::Cow::Borrowed(FEATURES.get_or_init(enabled_features))
}

/// The `--cfg feature=".."` values of a rustc command line; `None` outside
/// rustc (rust-analyzer, unit tests), where no feature set is known.
fn enabled_features() -> Option<Vec<String>> {
    let args = crate::env_access::compiler_args();
    args.iter().any(|arg| arg == "--crate-name").then(|| {
        args.windows(2)
            .filter(|pair| pair[0] == "--cfg")
            .filter_map(|pair| pair[1].strip_prefix("feature="))
            .map(|value| value.trim_matches('"').to_string())
            .collect()
    })
}

fn eval(predicate: &Predicate, features: Option<&[String]>) -> Option<bool> {
    let predicate = match predicate {
        Predicate::Bool(value) => return Some(value.value),
        Predicate::Meta(meta) => &**meta,
    };
    match predicate {
        syn::Meta::List(list) => {
            let operands = list
                .parse_args_with(Punctuated::<Predicate, syn::Token![,]>::parse_terminated)
                .ok()?;
            let mut values = operands.iter().map(|operand| eval(operand, features));
            if list.path.is_ident("all") {
                values
                    .try_fold(Some(true), |all, value| match value {
                        Some(false) => Err(()),
                        value => Ok(all.and(value)),
                    })
                    .unwrap_or(Some(false))
            } else if list.path.is_ident("any") {
                values
                    .try_fold(Some(false), |any, value| match value {
                        Some(true) => Err(()),
                        value => Ok(any.and(value)),
                    })
                    .unwrap_or(Some(true))
            } else if list.path.is_ident("not") && operands.len() == 1 {
                values.next().flatten().map(|value| !value)
            } else {
                None
            }
        }
        syn::Meta::NameValue(pair) if pair.path.is_ident("feature") => {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(value),
                ..
            }) = &pair.value
            else {
                return None;
            };
            features.map(|features| features.contains(&value.value()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::empty_any("any()", Some(false))]
    #[case::empty_all("all()", Some(true))]
    #[case::not_all("not(all())", Some(false))]
    #[case::unknown("unix", None)]
    #[case::unknown_any("any(unix, any())", None)]
    #[case::true_any("any(unix, all())", Some(true))]
    #[case::false_all("all(unix, any())", Some(false))]
    #[case::unknown_all("all(unix, all())", None)]
    #[case::not_unknown("not(unix)", None)]
    #[case::not_two_operands("not(unix, test)", None)]
    #[case::other_list("target(unix)", None)]
    #[case::enabled_feature("feature = \"pg\"", Some(true))]
    #[case::disabled_feature("not(feature = \"pg\")", Some(false))]
    #[case::missing_feature("feature = \"mysql\"", Some(false))]
    #[case::non_string_feature("feature = 1", None)]
    #[case::other_pair("target_os = \"linux\"", None)]
    #[case::literal_true("true", Some(true))]
    #[case::literal_false("false", Some(false))]
    #[case::not_literal_false("not(false)", Some(true))]
    #[case::any_literal_true("any(unix, true)", Some(true))]
    fn evaluates_known_predicates(#[case] predicate: &str, #[case] expected: Option<bool>) {
        let parsed: Predicate = syn::parse_str(predicate).unwrap();
        // The predicate prints back as written (it is re-emitted in `cfg_attr`).
        assert_eq!(
            quote::quote!(#parsed).to_string().replace(' ', ""),
            predicate.replace(' ', "")
        );
        assert_eq!(eval(&parsed, Some(&["pg".to_string()])), expected);
    }

    #[rstest]
    #[case::cfg_false("#[cfg(any())]", true)]
    #[case::cfg_true("#[cfg(all())]", false)]
    #[case::cfg_attr_false("#[cfg_attr(all(), cfg(any()))]", true)]
    #[case::cfg_attr_true("#[cfg_attr(all(), cfg(all()))]", false)]
    #[case::cfg_attr_unknown_predicate("#[cfg_attr(unix, cfg(any()))]", false)]
    #[case::cfg_attr_without_cfg("#[cfg_attr(all(), derive(Debug))]", false)]
    #[case::cfg_attr_false_among_active("#[cfg(all())] #[cfg_attr(all(), cfg(any()))]", true)]
    fn compiled_out_includes_cfg_attr_cfgs(#[case] attrs: &str, #[case] expected: bool) {
        let attrs = syn::parse::Parser::parse_str(syn::Attribute::parse_outer, attrs).unwrap();
        assert_eq!(compiled_out(&attrs), expected);
    }

    #[test]
    fn literal_cfg_is_evaluated() {
        assert!(statically_inactive(&[syn::parse_quote!(#[cfg(false)])]));
        assert!(!statically_inactive(&[syn::parse_quote!(#[cfg(true)])]));
    }

    #[test]
    fn unparseable_list_is_unknown() {
        let predicate: Predicate = syn::parse_str("any(= 1)").unwrap();
        assert_eq!(eval(&predicate, None), None);
    }

    #[test]
    fn features_are_known_only_inside_rustc() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[cfg(feature = "pg")])];
        let _outside = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "target/debug/deps/tests",
        );
        assert_eq!(enabled_features(), None);
        assert!(!statically_inactive(&attrs));
        let _rustc = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--crate-name\napp\n--cfg\nfeature=\"orm\"",
        );
        assert_eq!(enabled_features(), Some(vec!["orm".to_string()]));
        assert!(statically_inactive(&attrs));
        // Not a `cfg`, or a predicate that does not parse: kept.
        let kept: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[doc = "x"]),
            syn::parse_quote!(#[cfg(= 1)]),
        ];
        assert!(!statically_inactive(&kept));
    }
}
