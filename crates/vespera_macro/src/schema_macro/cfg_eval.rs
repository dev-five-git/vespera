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

use syn::punctuated::Punctuated;

/// Whether one of `attrs`' `#[cfg(..)]` predicates is known to be false.
pub fn statically_inactive(attrs: &[syn::Attribute]) -> bool {
    let features = enabled_features();
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| eval(&predicate, features.as_deref()) == Some(false))
    })
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

fn eval(predicate: &syn::Meta, features: Option<&[String]>) -> Option<bool> {
    match predicate {
        syn::Meta::List(list) => {
            let operands = list
                .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .ok()?;
            let mut values = operands.iter().map(|operand| eval(operand, features));
            if list.path.is_ident("all") {
                values.try_fold(Some(true), |all, value| match value {
                    Some(false) => Err(()),
                    value => Ok(all.and(value)),
                })
                .unwrap_or(Some(false))
            } else if list.path.is_ident("any") {
                values.try_fold(Some(false), |any, value| match value {
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
    fn evaluates_known_predicates(#[case] predicate: &str, #[case] expected: Option<bool>) {
        let predicate: syn::Meta = syn::parse_str(predicate).unwrap();
        assert_eq!(eval(&predicate, Some(&["pg".to_string()])), expected);
    }

    #[test]
    fn unparseable_list_is_unknown() {
        let predicate: syn::Meta = syn::parse_str("any(= 1)").unwrap();
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
        let kept: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[doc = "x"]), syn::parse_quote!(#[cfg(= 1)])];
        assert!(!statically_inactive(&kept));
    }
}
