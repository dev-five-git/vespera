//! A file parse that skips the bodies of top-level functions.
//!
//! Handler bodies are the bulk of a route file's token tree, and callers that
//! only read item names, `use` trees and function signatures (the extractor
//! validation) never look inside them.  A top-level `fn` is parsed as its
//! attributes, visibility and signature; its `{ .. }` body is consumed as one
//! token group and replaced by an empty block — the same trick
//! `route_impl::RouteFnHead` uses.  Every other item (inline modules
//! included) is parsed in full.

use syn::parse::{Parse, ParseStream};

struct ShallowFile(syn::File);

impl Parse for ShallowFile {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_inner)?;
        let mut items = Vec::new();
        while !input.is_empty() {
            items.push(shallow_item(input)?);
        }
        Ok(Self(syn::File {
            shebang: None,
            frontmatter: None,
            attrs,
            items,
        }))
    }
}

/// One item: a function with its body skipped when it has the plain
/// `attrs vis signature { .. }` shape, any other item in full.
fn shallow_item(input: ParseStream<'_>) -> syn::Result<syn::Item> {
    let fork = input.fork();
    let head = (|| {
        let attrs = fork.call(syn::Attribute::parse_outer)?;
        let vis: syn::Visibility = fork.parse()?;
        let sig: syn::Signature = fork.parse()?;
        let body;
        let brace_token = syn::braced!(body in fork);
        body.parse::<proc_macro2::TokenStream>()?;
        Ok::<_, syn::Error>(syn::ItemFn {
            attrs,
            vis,
            modifiers: syn::FnModifiers::default(),
            sig,
            block: Box::new(syn::Block {
                brace_token,
                stmts: Vec::new(),
            }),
        })
    })();
    if let Ok(function) = head {
        syn::parse::discouraged::Speculative::advance_to(input, &fork);
        return Ok(syn::Item::Fn(function));
    }
    input.parse()
}

/// `content` parsed with top-level function bodies skipped (see the module
/// docs), or in full by `syn::parse_file` when the shallow parse fails (a
/// shebang / frontmatter, which only `parse_file` strips, or a real syntax
/// error); `None` when neither parses.
pub fn parse_shallow(content: &str) -> Option<syn::File> {
    syn::parse_str::<ShallowFile>(content)
        .map(|file| file.0)
        .or_else(|_| syn::parse_file(content))
        .ok()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// The shallow parse keeps every item and signature and empties only the
    /// top-level function bodies.
    #[test]
    fn keeps_items_and_signatures() {
        let source = "#![allow(dead_code)]\nuse crate::a::B;\n#[route(get)]\npub async fn list(q: Query<B>) -> Json<B> { let x = 1; todo!() }\nstruct S;\nmod inner { pub fn f() -> u8 { 1 } }\nmacro_rules! m { () => {} }\nconst fn c() {}\nextern \"C\" fn e() {}\nimpl S { fn g(&self) {} }\n";
        let shallow = parse_shallow(source).unwrap();
        let full = syn::parse_file(source).unwrap();
        assert_eq!(shallow.attrs.len(), 1);
        assert_eq!(shallow.items.len(), full.items.len());
        for (shallow, full) in shallow.items.iter().zip(&full.items) {
            match (shallow, full) {
                (syn::Item::Fn(shallow), syn::Item::Fn(full)) => {
                    let (a, b) = (&shallow.sig, &full.sig);
                    assert_eq!(quote::quote!(#a).to_string(), quote::quote!(#b).to_string());
                    assert_eq!(shallow.attrs.len(), full.attrs.len());
                    assert_eq!(shallow.block.stmts.len(), 0);
                }
                (shallow, full) => assert_eq!(
                    quote::quote!(#shallow).to_string(),
                    quote::quote!(#full).to_string()
                ),
            }
        }
    }

    #[rstest]
    #[case::shebang("#!/usr/bin/env run\nfn a() {}", Some(1))]
    #[case::shebang_only("#!/usr/bin/env run", Some(0))]
    #[case::bom("\u{feff}fn a() {}", Some(1))]
    #[case::inner_attr_is_no_shebang("#![allow(x)] fn a() {}", Some(1))]
    #[case::broken("fn a() {", None)]
    #[case::broken_item("struct {", None)]
    fn handles_prefixes_and_errors(#[case] source: &str, #[case] items: Option<usize>) {
        assert_eq!(parse_shallow(source).map(|file| file.items.len()), items);
    }
}
