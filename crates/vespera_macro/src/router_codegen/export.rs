use syn::{
    LitStr,
    parse::{Parse, ParseStream},
};

/// Input for `export_app`! macro
pub struct ExportAppInput {
    /// App name (struct name to generate)
    pub name: syn::Ident,
    /// Route directory
    pub dir: Option<LitStr>,
}

impl Parse for ExportAppInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;

        let mut dir = None;

        // Parse optional comma and arguments
        while input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: syn::Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "dir" => {
                    // Reject a repeated `dir` with a spanned error instead of
                    // silently letting the later value overwrite the earlier
                    // one — matches the `vespera!` arg parser's duplicate guard.
                    if dir.is_some() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "duplicate field `dir` in export_app! macro",
                        ));
                    }
                    input.parse::<syn::Token![=]>()?;
                    dir = Some(input.parse()?);
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown field: `{ident_str}`. Expected `dir`"),
                    ));
                }
            }
        }

        Ok(Self { name, dir })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_app_input_name_only() {
        let tokens = quote::quote!(MyApp);
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert!(input.dir.is_none());
    }

    #[test]
    fn test_export_app_input_with_dir() {
        let tokens = quote::quote!(MyApp, dir = "api");
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert_eq!(input.dir.unwrap().value(), "api");
    }

    #[test]
    fn test_export_app_input_with_trailing_comma() {
        let tokens = quote::quote!(MyApp,);
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert!(input.dir.is_none());
    }

    #[test]
    fn test_export_app_input_unknown_field() {
        let tokens = quote::quote!(MyApp, unknown = "value");
        let result: syn::Result<ExportAppInput> = syn::parse2(tokens);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_compile_error().to_string().contains("unknown field"));
    }

    #[test]
    fn test_export_app_input_multiple_commas() {
        let tokens = quote::quote!(MyApp, dir = "api",);
        let input: ExportAppInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.name.to_string(), "MyApp");
        assert_eq!(input.dir.unwrap().value(), "api");
    }

    #[test]
    fn test_export_app_input_duplicate_dir() {
        // A repeated `dir` must be a spanned compile error, not a silent
        // last-wins overwrite.
        let tokens = quote::quote!(MyApp, dir = "api", dir = "other");
        let result: syn::Result<ExportAppInput> = syn::parse2(tokens);
        assert!(result.is_err(), "duplicate `dir` must be rejected");
        assert!(
            result
                .err()
                .unwrap()
                .to_compile_error()
                .to_string()
                .contains("duplicate field `dir`")
        );
    }
}
