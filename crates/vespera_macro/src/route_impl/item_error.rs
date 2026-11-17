//! The error for an item `#[route]` cannot parse as a function.

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// The error for an item `#[route]` cannot parse as a function.
///
/// An item that starts like a function (see [`starts_like_fn`]) keeps syn's own
/// message, which names the real fault (a missing comma in the parameter list,
/// a trailing token). Any other item is reported as not a function.
pub(super) fn route_item_error(item: &TokenStream, error: &syn::Error) -> syn::Error {
    if starts_like_fn(item.clone()) {
        syn::Error::new(error.span(), format!("#[route] attribute: {error}"))
    } else {
        syn::Error::new(
            error.span(),
            "#[route] attribute: can only be applied to functions, not other items. Move or remove the attribute.",
        )
    }
}

/// Whether `item` begins like a function declaration: after its attributes,
/// visibility and qualifiers (`pub(..)`, `async`, `const`, `unsafe`,
/// `default`, `extern "abi"`) the keyword is `fn`. A `fn` further on, in a
/// type such as `type H = fn(u8);`, does not make it a function.
fn starts_like_fn(item: TokenStream) -> bool {
    let mut tokens = item.into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            TokenTree::Punct(punct) if punct.as_char() == '#' => {
                tokens.next();
            }
            TokenTree::Ident(ident) => match ident.to_string().as_str() {
                "fn" => return true,
                "pub" => {
                    if matches!(tokens.peek(), Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis)
                    {
                        tokens.next();
                    }
                }
                "async" | "const" | "unsafe" | "default" | "extern" => {}
                _ => return false,
            },
            TokenTree::Literal(_) => {}
            TokenTree::Punct(_) | TokenTree::Group(_) => return false,
        }
    }
    false
}
