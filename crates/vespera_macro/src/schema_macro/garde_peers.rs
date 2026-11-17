//! `#[garde(..)]` rules naming a sibling field: garde_derive emits
//! `matches(peer)` as `&self.peer` (compared through `PartialEq`), and a
//! `custom(f(&self.peer))` / `if(cond = self.peer ..)` expression reads the
//! field as written.  `schema_type!` renames the peer with `rename`
//! (rewritten here), can drop it (`omit` / `pick` / `omit_default`: an
//! E0609 in the generated code) or wrap only the peer in `Option`
//! (`partial`: `matches` then compares `T` with `Option<T>`, E0277) — both
//! are reported as `schema_type!` diagnostics naming the parameter.

use std::collections::{HashMap, HashSet};

use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use syn::ext::IdentExt;

use super::module_scope::segment_ident;
use super::validation::ErrorAt;

/// One sibling field a garde rule of a generated field names.
pub struct PeerRef {
    /// The (source) field the rule sits on.
    pub field: String,
    /// Whether `partial` wraps that field in `Option`.
    pub field_wrapped: bool,
    /// The (source) field the rule names.
    pub peer: String,
    /// `matches(peer)` (else a `self.peer` expression).
    pub matches: bool,
}

/// A `garde(..)` meta with the `matches(peer)` / `self.peer` names `rename`
/// changes rewritten; every name is pushed onto `refs` as `(peer, matches)`.
pub fn rename_garde_peers(
    meta: &syn::Meta,
    rename_map: &HashMap<String, String>,
    refs: &mut Vec<(String, bool)>,
) -> syn::Meta {
    let syn::Meta::List(list) = meta else {
        return meta.clone();
    };
    let tokens = rename_tokens(list.tokens.clone(), rename_map, refs);
    let path = &list.path;
    syn::parse_quote!(#path(#tokens))
}

/// [`rename_garde_peers`] over a token stream (groups walked).
fn rename_tokens(
    tokens: TokenStream,
    rename_map: &HashMap<String, String>,
    refs: &mut Vec<(String, bool)>,
) -> TokenStream {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    let renamed = |ident: &syn::Ident| {
        rename_map
            .get(&ident.unraw().to_string())
            .map_or_else(|| ident.clone(), |name| segment_ident(name))
    };
    let mut out = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        match (&tokens[index], tokens.get(index + 1), tokens.get(index + 2)) {
            (TokenTree::Ident(rule), Some(TokenTree::Group(args)), _)
                if rule == "matches" && args.delimiter() == Delimiter::Parenthesis =>
            {
                if let Ok(peer) = syn::parse2::<syn::Ident>(args.stream()) {
                    refs.push((peer.unraw().to_string(), true));
                    let mut group = Group::new(
                        Delimiter::Parenthesis,
                        TokenTree::Ident(renamed(&peer)).into(),
                    );
                    group.set_span(args.span());
                    out.extend([tokens[index].clone(), group.into()]);
                    index += 2;
                    continue;
                }
            }
            (
                TokenTree::Ident(receiver),
                Some(TokenTree::Punct(dot)),
                Some(TokenTree::Ident(peer)),
            ) if receiver == "self" && dot.as_char() == '.' => {
                refs.push((peer.unraw().to_string(), false));
                out.extend([
                    tokens[index].clone(),
                    tokens[index + 1].clone(),
                    renamed(peer).into(),
                ]);
                index += 3;
                continue;
            }
            (TokenTree::Group(group), _, _) => {
                let mut rewritten = Group::new(
                    group.delimiter(),
                    rename_tokens(group.stream(), rename_map, refs),
                );
                rewritten.set_span(group.span());
                out.push(rewritten.into());
                index += 1;
                continue;
            }
            _ => {}
        }
        out.push(tokens[index].clone());
        index += 1;
    }
    out.into_iter().collect()
}

/// What decides whether a named peer still fits the generated type.
pub struct PeerContext<'a> {
    pub source_fields: &'a HashSet<String>,
    /// The source fields the type emits.
    pub emitted: &'a HashSet<String>,
    pub omit: &'a HashSet<String>,
    pub pick: &'a HashSet<String>,
    /// Kept by `omit` / `pick` yet not emitted, with the reason.
    pub dropped: &'a HashMap<String, &'static str>,
    /// The source fields `partial` wraps in `Option`.
    pub wrapped: &'a HashSet<String>,
}

/// An error for the first rule whose peer the type drops, or whose
/// `matches` compares an unwrapped field with a `partial`-wrapped peer.
pub fn check_garde_peers(
    refs: &[PeerRef],
    context: &PeerContext<'_>,
    at: &impl ErrorAt,
) -> syn::Result<()> {
    for reference in refs {
        let (field, peer) = (&reference.field, &reference.peer);
        if !context.source_fields.contains(peer) {
            continue;
        }
        let rule = if reference.matches {
            format!("`#[garde(matches({peer}))]`")
        } else {
            format!("the `#[garde(..)]` expression reading `self.{peer}`")
        };
        if !context.emitted.contains(peer) {
            let (param, reason) = if context.omit.contains(peer) {
                ("omit", "`omit` drops it")
            } else if !context.pick.is_empty() && !context.pick.contains(peer) {
                ("pick", "`pick` does not keep it")
            } else {
                (
                    "omit_default",
                    context
                        .dropped
                        .get(peer)
                        .copied()
                        .unwrap_or("this type does not include it"),
                )
            };
            return Err(at.error_at(param, peer, format!("{rule} on field `{field}` names field `{peer}`, which this type does not include ({reason}): keep `{peer}` or omit `{field}`")));
        }
        if context.wrapped.contains(peer) {
            // `matches(peer)` compares the two values, so both must be
            // `Option`s; a `self.peer` expression reads the peer as written
            // (`&Option<T>` where `T` is expected) whatever the rule field is.
            if reference.matches && reference.field_wrapped {
                continue;
            }
            let message = if reference.matches {
                format!(
                    "{rule} on field `{field}` compares it with `{peer}`, which `partial` wraps in `Option` while `{field}` stays required: make `{field}` partial too, or omit it"
                )
            } else {
                format!(
                    "{rule} on field `{field}` reads `self.{peer}`, which `partial` wraps in `Option` (the expression sees `Option<T>`): drop `{peer}` from `partial`, or omit `{field}`"
                )
            };
            return Err(at.error_at("partial", peer, message));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::matches_renamed(
        quote!(garde(matches(password))),
        "garde (matches (pw))",
        &[("password", true)]
    )]
    #[case::raw_matches(
        quote!(garde(length(min = 1), matches(r#type))),
        "garde (length (min = 1) , matches (r#type))",
        &[("type", true)]
    )]
    #[case::self_access_renamed(
        quote!(garde(custom(check(&self.password)), if(cond = self.password.is_empty(), length(min = 1)))),
        "garde (custom (check (& self . pw)) , if (cond = self . pw . is_empty () , length (min = 1)))",
        &[("password", false), ("password", false)]
    )]
    #[case::not_a_peer(
        quote!(garde(matches(a, b), custom(self::check), length(max = MAX))),
        "garde (matches (a , b) , custom (self :: check) , length (max = MAX))",
        &[]
    )]
    fn rewrites_renamed_peers(
        #[case] meta: TokenStream,
        #[case] expected: &str,
        #[case] expected_refs: &[(&str, bool)],
    ) {
        let meta: syn::Meta = syn::parse2(meta).unwrap();
        let rename_map: HashMap<String, String> =
            [("password".to_string(), "pw".to_string())].into();
        let mut refs = Vec::new();
        let renamed = rename_garde_peers(&meta, &rename_map, &mut refs);
        assert_eq!(quote!(#renamed).to_string(), expected);
        let refs: Vec<(&str, bool)> = refs.iter().map(|(peer, m)| (peer.as_str(), *m)).collect();
        assert_eq!(refs, expected_refs);
    }

    #[test]
    fn a_path_meta_is_kept() {
        let meta: syn::Meta = syn::parse_quote!(garde);
        let mut refs = Vec::new();
        let kept = rename_garde_peers(&meta, &HashMap::new(), &mut refs);
        assert_eq!(quote!(#kept).to_string(), "garde");
        assert_eq!(refs.len(), 0);
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    /// `confirm` names `password` (`matches` or `self.password`); the type
    /// is shaped by `omit` / `pick` / `partial` / `omit_default`.
    #[rstest]
    #[case::kept(true, set(&["password", "confirm"]), set(&[]), set(&[]), set(&[]), None)]
    #[case::omitted(true, set(&["confirm"]), set(&["password"]), set(&[]), set(&[]), Some("`omit` drops it"))]
    #[case::not_picked(false, set(&["confirm"]), set(&[]), set(&["confirm"]), set(&[]), Some("`pick` does not keep it"))]
    #[case::omit_default(false, set(&["confirm"]), set(&[]), set(&[]), set(&[]), Some("a SeaORM default"))]
    #[case::partial_peer(true, set(&["password", "confirm"]), set(&[]), set(&[]), set(&["password"]), Some("which `partial` wraps in `Option` while `confirm` stays required"))]
    #[case::partial_self_access(false, set(&["password", "confirm"]), set(&[]), set(&[]), set(&["password"]), Some("reads `self.password`, which `partial` wraps in `Option`"))]
    fn checks_named_peers(
        #[case] matches: bool,
        #[case] emitted: HashSet<String>,
        #[case] omit: HashSet<String>,
        #[case] pick: HashSet<String>,
        #[case] wrapped: HashSet<String>,
        #[case] error: Option<&str>,
    ) {
        let dropped: HashMap<String, &'static str> =
            [("password".to_string(), "a SeaORM default")].into();
        let context = PeerContext {
            source_fields: &set(&["password", "confirm"]),
            emitted: &emitted,
            omit: &omit,
            pick: &pick,
            dropped: &dropped,
            wrapped: &wrapped,
        };
        let refs = [
            PeerRef {
                field: "confirm".into(),
                field_wrapped: false,
                peer: "unknown".into(),
                matches,
            },
            PeerRef {
                field: "confirm".into(),
                field_wrapped: false,
                peer: "password".into(),
                matches,
            },
        ];
        let at: syn::Type = syn::parse_quote!(Source);
        let result = check_garde_peers(&refs, &context, &at);
        match error {
            None => assert!(result.is_ok()),
            Some(message) => {
                let err = result.unwrap_err().to_string();
                assert!(err.contains(message), "{err}");
            }
        }
    }

    /// Only `matches` between two wrapped fields compares `Option`s; a
    /// `self.peer` read sees `Option<T>` whatever the rule field is.
    #[rstest]
    #[case::matches_both_wrapped(true, true, false)]
    #[case::matches_plain_field(true, false, true)]
    #[case::self_read_wrapped_field(false, true, true)]
    #[case::self_read_plain_field(false, false, true)]
    fn wrapped_peer_rules(#[case] matches: bool, #[case] field_wrapped: bool, #[case] fails: bool) {
        let all: HashSet<String> = set(&["password", "confirm"]);
        let wrapped: HashSet<String> = set(&["password"]);
        let none: HashSet<String> = HashSet::new();
        let dropped: HashMap<String, &'static str> = HashMap::new();
        let context = PeerContext {
            source_fields: &all,
            emitted: &all,
            omit: &none,
            pick: &none,
            dropped: &dropped,
            wrapped: &wrapped,
        };
        let refs = [PeerRef {
            field: "confirm".into(),
            field_wrapped,
            peer: "password".into(),
            matches,
        }];
        let at: syn::Type = syn::parse_quote!(Source);
        assert_eq!(check_garde_peers(&refs, &context, &at).is_err(), fails);
    }

    /// A wrapped field's `matches` (moved into `inner(..)` or comparing two
    /// `Option`s) fits whatever the peer is; a dropped peer without a
    /// recorded reason still names the field.
    #[test]
    fn wrapped_matches_and_unknown_drops() {
        let empty = HashSet::new();
        let context = PeerContext {
            source_fields: &set(&["password", "confirm"]),
            emitted: &set(&["password", "confirm"]),
            omit: &empty,
            pick: &empty,
            dropped: &HashMap::new(),
            wrapped: &set(&["password"]),
        };
        let wrapped = PeerRef {
            field: "confirm".into(),
            field_wrapped: true,
            peer: "password".into(),
            matches: true,
        };
        let at: syn::Type = syn::parse_quote!(Source);
        assert!(check_garde_peers(&[wrapped], &context, &at).is_ok());
        let context = PeerContext {
            emitted: &empty,
            ..context
        };
        let reference = PeerRef {
            field: "confirm".into(),
            field_wrapped: false,
            peer: "password".into(),
            matches: false,
        };
        let err = check_garde_peers(&[reference], &context, &at)
            .unwrap_err()
            .to_string();
        assert!(err.contains("this type does not include it"), "{err}");
    }
}
