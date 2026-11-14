use crate::http::is_http_method;
use crate::metadata::HeaderParam;
use syn::{LitBool, LitInt, LitStr, bracketed};

pub struct RouteArgs {
    pub method: Option<syn::Ident>,
    pub path: Option<syn::LitStr>,
    pub error_status: Option<syn::ExprArray>,
    pub responses: Option<syn::ExprArray>,
    /// Declared non-200 success status from `status = <u16>` (validated 2xx).
    pub success_status: Option<u16>,
    pub tags: Option<syn::ExprArray>,
    pub security: Option<syn::ExprArray>,
    pub headers: Option<Vec<HeaderParam>>,
    pub operation_id: Option<syn::LitStr>,
    pub summary: Option<syn::LitStr>,
    pub request_example: Option<syn::LitStr>,
    pub response_example: Option<syn::LitStr>,
    pub deprecated: bool,
    pub description: Option<syn::LitStr>,
}

impl syn::parse::Parse for RouteArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut args = RouteArgsBuilder::default();

        // Parse comma-separated list of arguments
        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                // Try to parse as method identifier (get, post, etc.)
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string().to_lowercase();
                args.parse_ident(input, &ident, &ident_str)?;

                // Check if there's a comma
                if input.peek(syn::Token![,]) {
                    input.parse::<syn::Token![,]>()?;
                } else {
                    break;
                }
            } else {
                return Err(lookahead.error());
            }
        }

        Ok(args.finish())
    }
}

#[derive(Default)]
struct RouteArgsBuilder {
    method: Option<syn::Ident>,
    path: Option<syn::LitStr>,
    error_status: Option<syn::ExprArray>,
    responses: Option<syn::ExprArray>,
    success_status: Option<u16>,
    tags: Option<syn::ExprArray>,
    security: Option<syn::ExprArray>,
    headers: Option<Vec<HeaderParam>>,
    operation_id: Option<syn::LitStr>,
    summary: Option<syn::LitStr>,
    request_example: Option<syn::LitStr>,
    response_example: Option<syn::LitStr>,
    deprecated: bool,
    description: Option<syn::LitStr>,
}

/// The `#[route(...)]` keys besides the HTTP method names.
const ROUTE_KEYS: [&str; 13] = [
    "path",
    "error_status",
    "responses",
    "status",
    "tags",
    "security",
    "headers",
    "operation_id",
    "summary",
    "request_example",
    "response_example",
    "deprecated",
    "description",
];

/// Spanned error for an unknown `#[route]` key, listing the accepted ones.
fn unknown_argument(ident: &syn::Ident) -> syn::Error {
    let methods: Vec<String> = vespera_core::route::HttpMethod::ALL
        .iter()
        .map(|method| method.as_str().to_ascii_lowercase())
        .collect();
    syn::Error::new(
        ident.span(),
        format!(
            "unknown #[route] argument `{ident}`; expected an HTTP method ({}) or one of `{}`",
            methods.join(", "),
            ROUTE_KEYS.join("`, `"),
        ),
    )
}

impl RouteArgsBuilder {
    fn parse_ident(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
        ident_str: &str,
    ) -> syn::Result<()> {
        if is_http_method(ident_str) {
            return self.parse_method(ident);
        }
        match ident_str {
            "path" => self.parse_path(input, ident),
            "error_status" => self.parse_error_status(input, ident),
            "responses" => self.parse_responses(input, ident),
            "status" => self.parse_status(input, ident),
            "tags" => self.parse_tags(input, ident),
            "security" => self.parse_security(input, ident),
            "headers" => self.parse_headers(input, ident),
            "operation_id" => self.parse_operation_id(input, ident),
            "summary" => self.parse_summary(input, ident),
            "request_example" => self.parse_request_example(input, ident),
            "response_example" => self.parse_response_example(input, ident),
            "deprecated" => self.parse_deprecated(ident),
            "description" => self.parse_description(input, ident),
            _ => Err(unknown_argument(ident)),
        }
    }

    fn parse_method(&mut self, ident: &syn::Ident) -> syn::Result<()> {
        reject_duplicate(self.method.as_ref(), ident, "HTTP method")?;
        self.method = Some(ident.clone());
        Ok(())
    }

    fn parse_path(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.path.as_ref(), ident, "path")?;
        input.parse::<syn::Token![=]>()?;
        self.path = Some(crate::parser::normalize_route_path(&input.parse()?)?);
        Ok(())
    }

    fn parse_error_status(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.error_status.as_ref(), ident, "error_status")?;
        input.parse::<syn::Token![=]>()?;
        let array: syn::ExprArray = input.parse()?;
        validate_error_status_array(&array)?;
        self.error_status = Some(array);
        Ok(())
    }

    fn parse_responses(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.responses.as_ref(), ident, "responses")?;
        input.parse::<syn::Token![=]>()?;
        let array: syn::ExprArray = input.parse()?;
        validate_responses_array(&array)?;
        self.responses = Some(array);
        Ok(())
    }

    fn parse_status(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.success_status.as_ref(), ident, "status")?;
        input.parse::<syn::Token![=]>()?;
        let lit: LitInt = input.parse()?;
        let code = lit.base10_parse::<u16>()?;
        if !(200..300).contains(&code) {
            return Err(syn::Error::new(
                lit.span(),
                "#[route] `status` must be a 2xx success status code (200-299).",
            ));
        }
        self.success_status = Some(code);
        Ok(())
    }

    fn parse_tags(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.tags.as_ref(), ident, "tags")?;
        input.parse::<syn::Token![=]>()?;
        self.tags = Some(input.parse()?);
        Ok(())
    }

    fn parse_security(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.security.as_ref(), ident, "security")?;
        input.parse::<syn::Token![=]>()?;
        self.security = Some(input.parse()?);
        Ok(())
    }

    fn parse_headers(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        reject_duplicate(self.headers.as_ref(), ident, "headers")?;
        self.headers = Some(parse_header_values(input)?);
        Ok(())
    }

    fn parse_lit_str_slot(
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
        slot: &mut Option<syn::LitStr>,
        name: &str,
    ) -> syn::Result<()> {
        reject_duplicate(slot.as_ref(), ident, name)?;
        input.parse::<syn::Token![=]>()?;
        *slot = Some(input.parse()?);
        Ok(())
    }

    fn parse_operation_id(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.operation_id, "operation_id")
    }

    fn parse_summary(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.summary, "summary")
    }

    fn parse_request_example(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.request_example, "request_example")
    }

    fn parse_response_example(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.response_example, "response_example")
    }

    fn parse_deprecated(&mut self, ident: &syn::Ident) -> syn::Result<()> {
        if self.deprecated {
            return Err(duplicate_error(ident, "deprecated"));
        }
        self.deprecated = true;
        Ok(())
    }

    fn parse_description(
        &mut self,
        input: syn::parse::ParseStream,
        ident: &syn::Ident,
    ) -> syn::Result<()> {
        Self::parse_lit_str_slot(input, ident, &mut self.description, "description")
    }

    fn finish(self) -> RouteArgs {
        RouteArgs {
            method: self.method,
            path: self.path,
            error_status: self.error_status,
            responses: self.responses,
            success_status: self.success_status,
            tags: self.tags,
            security: self.security,
            headers: self.headers,
            operation_id: self.operation_id,
            summary: self.summary,
            request_example: self.request_example,
            response_example: self.response_example,
            deprecated: self.deprecated,
            description: self.description,
        }
    }
}

fn reject_duplicate<T>(slot: Option<&T>, ident: &syn::Ident, name: &str) -> syn::Result<()> {
    if slot.is_some() {
        Err(duplicate_error(ident, name))
    } else {
        Ok(())
    }
}

fn duplicate_error(ident: &syn::Ident, name: &str) -> syn::Error {
    syn::Error::new(
        ident.span(),
        format!("#[route] `{name}` specified more than once"),
    )
}

/// Validate `error_status = [<u16>, ...]`: every element must be an integer
/// literal naming an HTTP status code (`100..=599`).  A malformed entry is rejected with a
/// span-attached compile error instead of being silently dropped by the
/// downstream `filter_map` extraction (which would emit incomplete OpenAPI).
fn validate_error_status_array(array: &syn::ExprArray) -> syn::Result<()> {
    for elem in &array.elems {
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(lit_int),
            ..
        }) = elem
        else {
            return Err(syn::Error::new_spanned(
                elem,
                "#[route] `error_status` entries must be integer status codes, \
                 e.g. `error_status = [400, 404]`.",
            ));
        };
        parse_http_status(lit_int, "error_status")?;
    }
    Ok(())
}

/// An HTTP status code literal: `100..=599` (RFC 9110 section 15). Anything
/// else is not a valid OpenAPI response key and is rejected on its span.
fn parse_http_status(lit_int: &LitInt, key: &str) -> syn::Result<u16> {
    lit_int
        .base10_parse::<u16>()
        .ok()
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| {
            syn::Error::new_spanned(
                lit_int,
                format!("#[route] `{key}` status must be an HTTP status code (100-599)."),
            )
        })
}

/// Validate `responses = [(<u16>, Type), ...]`: every element must be a
/// `(status, Type)` tuple with a `u16` status literal and a type **path**.
/// Malformed entries (a bare `(404)` parenthesized expr, a wrong-arity tuple,
/// a non-integer status, or a non-path type) are rejected with a span-attached
/// compile error instead of being silently dropped by the downstream
/// `filter_map` extraction — which previously produced incomplete OpenAPI with
/// no diagnostic (e.g. `responses = [(404)]` parsed "successfully" and emitted
/// nothing).
fn validate_responses_array(array: &syn::ExprArray) -> syn::Result<()> {
    for elem in &array.elems {
        let syn::Expr::Tuple(tuple) = elem else {
            return Err(syn::Error::new_spanned(
                elem,
                "#[route] `responses` entries must be `(status, Type)` tuples, \
                 e.g. `responses = [(404, NotFoundError)]`.",
            ));
        };
        if tuple.elems.len() != 2 {
            return Err(syn::Error::new_spanned(
                tuple,
                "#[route] `responses` entry must be a `(status, Type)` tuple with \
                 exactly two elements, e.g. `(404, NotFoundError)`.",
            ));
        }
        let status = &tuple.elems[0];
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(lit_int),
            ..
        }) = status
        else {
            return Err(syn::Error::new_spanned(
                status,
                "#[route] `responses` status must be an integer literal, \
                 e.g. `(404, NotFoundError)`.",
            ));
        };
        parse_http_status(lit_int, "responses")?;
        let schema = &tuple.elems[1];
        if !matches!(schema, syn::Expr::Path(_)) {
            return Err(syn::Error::new_spanned(
                schema,
                "#[route] `responses` type must be a type path, \
                 e.g. `(404, NotFoundError)` or `(400, crate::errors::BadRequestError)`.",
            ));
        }
    }
    Ok(())
}

fn parse_header_values(input: syn::parse::ParseStream) -> syn::Result<Vec<HeaderParam>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let mut headers = Vec::new();

    while !content.is_empty() {
        headers.push(parse_header_struct(&content)?);

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    Ok(headers)
}

fn parse_header_struct(input: syn::parse::ParseStream) -> syn::Result<HeaderParam> {
    let content;
    syn::braced!(content in input);

    let mut name: Option<String> = None;
    let mut required = false;
    let mut required_seen = false;
    let mut description: Option<String> = None;

    while !content.is_empty() {
        let ident: syn::Ident = content.parse()?;
        let ident_str = ident.to_string();
        content.parse::<syn::Token![=]>()?;

        // Reject a repeated field in one header object instead of letting the
        // later value silently win (which produced ambiguous OpenAPI with no
        // diagnostic).  `required` is a bare `bool`, so it needs its own
        // seen-flag; `name`/`description` are `Option`s already.
        match ident_str.as_str() {
            "name" => {
                if name.is_some() {
                    return Err(syn::Error::new(
                        ident.span(),
                        "duplicate header field `name`",
                    ));
                }
                name = Some(content.parse::<LitStr>()?.value());
            }
            "required" => {
                if required_seen {
                    return Err(syn::Error::new(
                        ident.span(),
                        "duplicate header field `required`",
                    ));
                }
                required = content.parse::<LitBool>()?.value;
                required_seen = true;
            }
            "description" => {
                if description.is_some() {
                    return Err(syn::Error::new(
                        ident.span(),
                        "duplicate header field `description`",
                    ));
                }
                description = Some(content.parse::<LitStr>()?.value());
            }
            _ => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!(
                        "unknown header field: `{ident_str}`. Expected `name`, `required`, or `description`"
                    ),
                ));
            }
        }

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    let name = name.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[route] headers entry missing required `name` field.",
        )
    })?;

    Ok(HeaderParam {
        name,
        required,
        description,
    })
}

#[cfg(test)]
mod tests;
