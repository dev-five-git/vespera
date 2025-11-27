pub struct RouteArgs {
    pub method: Option<syn::Ident>,
    pub path: Option<syn::LitStr>,
    pub error_status: Option<syn::ExprArray>,
}

impl syn::parse::Parse for RouteArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut method: Option<syn::Ident> = None;
        let mut path: Option<syn::LitStr> = None;
        let mut error_status: Option<syn::ExprArray> = None;

        // Parse comma-separated list of arguments
        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                // Try to parse as method identifier (get, post, etc.)
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string().to_lowercase();
                match ident_str.as_str() {
                    "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                        method = Some(ident);
                    }
                    "path" => {
                        input.parse::<syn::Token![=]>()?;
                        let lit: syn::LitStr = input.parse()?;
                        path = Some(lit);
                    }
                    "error_status" => {
                        input.parse::<syn::Token![=]>()?;
                        let array: syn::ExprArray = input.parse()?;
                        error_status = Some(array);
                    }
                    _ => {
                        return Err(lookahead.error());
                    }
                }
            } else {
                return Err(lookahead.error());
            }

            // Check if there's a comma
            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(RouteArgs { method, path, error_status })
    }
}

