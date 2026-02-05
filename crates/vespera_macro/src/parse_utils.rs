//! Parsing utilities for proc-macro input.
//!
//! Provides reusable helpers for parsing common patterns in proc-macro inputs,
//! including lookahead-based parsing, key-value pairs, and bracket-delimited lists.
//!
//! These utilities are available for future refactoring of existing parsing code in `args.rs`
//! and `router_codegen.rs`. They extract the most common lookahead-based patterns.

#![allow(dead_code)]

use syn::{Ident, LitStr, Token, parse::ParseStream};

/// Parse a comma-separated list with optional trailing comma.
///
/// Automatically handles the lookahead and comma parsing loop.
/// The provided parser function is called for each item.
///
/// # Example
/// ```ignore
/// let items: Vec<String> = parse_comma_list(input, |input| {
///     input.parse::<LitStr>().map(|lit| lit.value())
/// })?;
/// ```
pub fn parse_comma_list<T, F>(input: ParseStream, mut parser: F) -> syn::Result<Vec<T>>
where
    F: FnMut(ParseStream) -> syn::Result<T>,
{
    let mut items = Vec::new();

    while !input.is_empty() {
        items.push(parser(input)?);

        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        } else {
            break;
        }
    }

    Ok(items)
}

/// Parse a bracket-delimited comma-separated list.
///
/// # Example
/// ```ignore
/// let items: Vec<String> = parse_bracketed_list(input, |input| {
///     input.parse::<LitStr>().map(|lit| lit.value())
/// })?;
/// ```
pub fn parse_bracketed_list<T, F>(input: ParseStream, parser: F) -> syn::Result<Vec<T>>
where
    F: Fn(ParseStream) -> syn::Result<T>,
{
    let content;
    syn::bracketed!(content in input);
    parse_comma_list(&content, parser)
}

/// Parse identifier-based key-value pairs.
///
/// Looks for patterns like `key = value`, where the key is an identifier.
/// Returns the key as a string and leaves the `=` token consumed.
///
/// # Returns
/// - `Some((key, true))` if we found an identifier that could be a key
/// - `None` if end of input or unexpected token type
///
/// # Example
/// ```ignore
/// if let Some((key, _)) = try_parse_key(input)? {
///     match key.as_str() {
///         "title" => { input.parse::<Token![=]>()?; title = Some(input.parse()?); }
///         "version" => { input.parse::<Token![=]>()?; version = Some(input.parse()?); }
///         _ => return Err(syn::Error::new(...))
///     }
/// }
/// ```
pub fn try_parse_key(input: ParseStream) -> syn::Result<Option<String>> {
    let lookahead = input.lookahead1();

    if lookahead.peek(Ident) {
        let ident: Ident = input.parse()?;
        Ok(Some(ident.to_string()))
    } else if lookahead.peek(LitStr) {
        Ok(None)
    } else {
        Err(lookahead.error())
    }
}

/// Parse a list of identifier-keyed key-value pairs.
///
/// Expects comma-separated key=value pairs where keys are identifiers.
/// Each iteration calls the handler with the key, and the handler is responsible
/// for consuming the `=` token and parsing the value.
///
/// # Example
/// ```ignore
/// let mut title = None;
/// let mut version = None;
///
/// parse_key_value_list(input, |key, input| {
///     match key.as_str() {
///         "title" => {
///             input.parse::<Token![=]>()?;
///             title = Some(input.parse()?);
///         }
///         "version" => {
///             input.parse::<Token![=]>()?;
///             version = Some(input.parse()?);
///         }
///         _ => return Err(syn::Error::new(...))
///     }
///     Ok(())
/// })?;
/// ```
pub fn parse_key_value_list<F>(input: ParseStream, mut handler: F) -> syn::Result<()>
where
    F: FnMut(String, ParseStream) -> syn::Result<()>,
{
    while !input.is_empty() {
        let lookahead = input.lookahead1();

        if lookahead.peek(Ident) {
            let ident: Ident = input.parse()?;
            let key = ident.to_string();
            handler(key, input)?;

            // Check if there's a comma
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else {
                break;
            }
        } else if lookahead.peek(LitStr) {
            // Allow string as a special case (e.g., for backward compatibility)
            break;
        } else {
            return Err(lookahead.error());
        }
    }

    Ok(())
}

/// Check if next token is a comma and consume it if present.
///
/// Returns `true` if comma was found and consumed, `false` otherwise.
pub fn try_consume_comma(input: ParseStream) -> bool {
    if input.peek(Token![,]) {
        let _ = input.parse::<Token![,]>();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use syn::parse::Parser;

    use super::*;

    #[test]
    fn test_parse_comma_list_single() {
        // Test basic parsing capability - parse a list of 3 strings
        let parser = |input: ParseStream| {
            parse_comma_list(input, |input| {
                input.parse::<LitStr>().map(|lit| lit.value())
            })
        };

        let tokens = quote::quote!("a", "b", "c");
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        let items: Vec<String> = result.unwrap();
        assert_eq!(items, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_comma_list_with_trailing_comma() {
        let parser = |input: ParseStream| {
            parse_comma_list(input, |input| {
                input.parse::<LitStr>().map(|lit| lit.value())
            })
        };

        let tokens = quote::quote!("x", "y",);
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        let items: Vec<String> = result.unwrap();
        assert_eq!(items, vec!["x", "y"]);
    }

    #[test]
    fn test_parse_bracketed_list_strings() {
        let parser = |input: ParseStream| {
            parse_bracketed_list(input, |input| {
                input.parse::<LitStr>().map(|lit| lit.value())
            })
        };

        let tokens = quote::quote!(["a", "b", "c"]);
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        let items: Vec<String> = result.unwrap();
        assert_eq!(items, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_try_parse_key_ident() {
        let parser = |input: ParseStream| try_parse_key(input);

        let tokens = quote::quote!(title);
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        let key = result.unwrap();
        assert_eq!(key, Some("title".to_string()));
    }

    #[test]
    fn test_try_consume_comma_logic() {
        // Test the comma consumption logic by parsing and manually checking
        let parser = |input: ParseStream| {
            let has_comma = try_consume_comma(input);
            Ok(has_comma)
        };

        let tokens = quote::quote!(,);
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_parse_key_value_handler() {
        let parser = |input: ParseStream| {
            let mut title = None;
            let mut version = None;

            parse_key_value_list(input, |key, input| {
                match key.as_str() {
                    "title" => {
                        input.parse::<Token![=]>()?;
                        title = Some(input.parse::<LitStr>()?.value());
                    }
                    "version" => {
                        input.parse::<Token![=]>()?;
                        version = Some(input.parse::<LitStr>()?.value());
                    }
                    _ => {
                        return Err(syn::Error::new(
                            proc_macro2::Span::call_site(),
                            "unknown key",
                        ));
                    }
                }
                Ok(())
            })?;

            Ok((title, version))
        };

        let tokens = quote::quote!(title = "Test", version = "1.0");
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        let (title, version) = result.unwrap();
        assert_eq!(title, Some("Test".to_string()));
        assert_eq!(version, Some("1.0".to_string()));
    }
}
