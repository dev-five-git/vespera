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
    fn test_try_parse_key_litstr() {
        // When input is a LitStr, try_parse_key returns Ok(None) without consuming
        let parser = |input: ParseStream| {
            let result = try_parse_key(input)?;
            // LitStr remains unconsumed, parse it to clear the buffer
            let _: LitStr = input.parse()?;
            Ok(result)
        };

        let tokens = quote::quote!("some_string");
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_try_parse_key_invalid_token() {
        // When input is neither Ident nor LitStr, try_parse_key returns error
        let parser = |input: ParseStream| try_parse_key(input);

        // Use a number literal which is neither Ident nor LitStr
        let tokens = quote::quote!(42);
        let result = parser.parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_try_consume_comma_no_comma() {
        // When there's no comma, try_consume_comma returns false without consuming
        let parser = |input: ParseStream| {
            let has_comma = try_consume_comma(input);
            // Token remains unconsumed, parse it to clear the buffer
            let _: Ident = input.parse()?;
            Ok(has_comma)
        };

        // Some token that's not a comma
        let tokens = quote::quote!(foo);
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        assert!(!result.unwrap());
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

    #[test]
    fn test_parse_key_value_list_litstr_breaks() {
        // When a LitStr is encountered in parse_key_value_list, it breaks (doesn't error)
        let parser = |input: ParseStream| {
            let mut keys = Vec::new();
            parse_key_value_list(input, |key, input| {
                input.parse::<Token![=]>()?;
                let _: LitStr = input.parse()?;
                keys.push(key);
                Ok(())
            })?;
            // The remaining LitStr is left unconsumed, parse it to clear the buffer
            let _: LitStr = input.parse()?;
            Ok(keys)
        };

        // "remaining" is a LitStr at the end, should break without error
        let tokens = quote::quote!(title = "Test", "remaining");
        let result = parser.parse2(tokens);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vec!["title"]);
    }

    #[test]
    fn test_parse_key_value_list_invalid_token() {
        // When an invalid token (not Ident or LitStr) is encountered, returns error
        let parser = |input: ParseStream| {
            parse_key_value_list(input, |_key, input| {
                input.parse::<Token![=]>()?;
                let _: LitStr = input.parse()?;
                Ok(())
            })
        };

        // 42 is neither Ident nor LitStr, should error
        let tokens = quote::quote!(42);
        let result = parser.parse2(tokens);
        assert!(result.is_err());
    }
}
