use proc_macro2::Span;
use std::collections::{BTreeMap, HashMap, HashSet};
use syn::{
    LitStr, bracketed,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};
use vespera_core::openapi::Server;
use vespera_core::schema::{SecurityScheme, SecuritySchemeType};

/// Server configuration for `OpenAPI`
#[derive(Clone)]
pub struct ServerConfig {
    pub url: String,
    pub description: Option<String>,
}

/// Security scheme configuration for `OpenAPI` components.
#[derive(Clone)]
pub struct SecuritySchemeConfig {
    pub name: String,
    pub scheme: SecurityScheme,
}

/// Top-level OpenAPI tag configuration from `vespera!(tags = [...])`.
#[derive(Clone)]
pub struct TagConfig {
    pub name: String,
    pub description: Option<String>,
}

/// Input for the `vespera!` macro
pub struct AutoRouterInput {
    pub dir: Option<LitStr>,
    pub openapi: Option<Vec<LitStr>>,
    pub title: Option<LitStr>,
    pub version: Option<LitStr>,
    pub docs_url: Option<LitStr>,
    pub redoc_url: Option<LitStr>,
    pub servers: Option<Vec<ServerConfig>>,
    pub security_schemes: Option<Vec<SecuritySchemeConfig>>,
    pub security: Option<Vec<String>>,
    pub tags: Option<Vec<TagConfig>>,
    /// Apps to merge (e.g., [`third::ThirdApp`, `another::AnotherApp`])
    pub merge: Option<Vec<syn::Path>>,
}

impl Parse for AutoRouterInput {
    #[allow(clippy::too_many_lines)]
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut dir = None;
        let mut openapi = None;
        let mut title = None;
        let mut version = None;
        let mut docs_url = None;
        let mut redoc_url = None;
        let mut servers = None;
        let mut security_schemes = None;
        let mut security = None;
        let mut tags = None;
        let mut merge = None;
        // Reject a repeated named argument (e.g. `title = ..., title = ...`)
        // with a spanned error instead of silently letting the later value
        // overwrite the earlier one — a typo would otherwise build a spec that
        // does not match the source.
        let mut seen_fields = HashSet::<String>::new();

        while !input.is_empty() {
            let lookahead = input.lookahead1();

            if lookahead.peek(syn::Ident) {
                let ident: syn::Ident = input.parse()?;
                let ident_str = ident.to_string();
                if !seen_fields.insert(ident_str.clone()) {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("duplicate field `{ident_str}` in vespera! macro"),
                    ));
                }

                match ident_str.as_str() {
                    "dir" => {
                        input.parse::<syn::Token![=]>()?;
                        dir = Some(input.parse()?);
                    }
                    "openapi" => {
                        openapi = Some(parse_openapi_values(input)?);
                    }
                    "docs_url" => {
                        input.parse::<syn::Token![=]>()?;
                        docs_url = Some(input.parse()?);
                    }
                    "redoc_url" => {
                        input.parse::<syn::Token![=]>()?;
                        redoc_url = Some(input.parse()?);
                    }
                    "title" => {
                        input.parse::<syn::Token![=]>()?;
                        title = Some(input.parse()?);
                    }
                    "version" => {
                        input.parse::<syn::Token![=]>()?;
                        version = Some(input.parse()?);
                    }
                    "servers" => {
                        servers = Some(parse_servers_values(input)?);
                    }
                    "security_schemes" => {
                        security_schemes = Some(parse_security_scheme_values(input)?);
                    }
                    "security" => {
                        security = Some(parse_security_values(input)?);
                    }
                    "tags" => {
                        tags = Some(parse_tag_values(input)?);
                    }
                    "merge" => {
                        merge = Some(parse_merge_values(input)?);
                    }
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown field: `{ident_str}`. Expected `dir`, `openapi`, `title`, `version`, `docs_url`, `redoc_url`, `servers`, `security_schemes`, `security`, `tags`, or `merge`"
                            ),
                        ));
                    }
                }
            } else if lookahead.peek(syn::LitStr) {
                // If just a string, treat it as dir (for backward compatibility)
                dir = Some(input.parse()?);
            } else {
                return Err(lookahead.error());
            }

            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(Self {
            dir: dir.or_else(|| {
                std::env::var("VESPERA_DIR")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            openapi: openapi.or_else(|| {
                std::env::var("VESPERA_OPENAPI")
                    .map(|f| vec![LitStr::new(&f, Span::call_site())])
                    .ok()
            }),
            title: title.or_else(|| {
                std::env::var("VESPERA_TITLE")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            version: version
                .or_else(|| {
                    std::env::var("VESPERA_VERSION")
                        .map(|f| LitStr::new(&f, Span::call_site()))
                        .ok()
                })
                .or_else(|| {
                    std::env::var("CARGO_PKG_VERSION")
                        .map(|f| LitStr::new(&f, Span::call_site()))
                        .ok()
                }),
            docs_url: docs_url.or_else(|| {
                std::env::var("VESPERA_DOCS_URL")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            redoc_url: redoc_url.or_else(|| {
                std::env::var("VESPERA_REDOC_URL")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            servers: servers.or_else(|| {
                std::env::var("VESPERA_SERVER_URL")
                    .ok()
                    .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
                    .map(|url| {
                        vec![ServerConfig {
                            url,
                            description: std::env::var("VESPERA_SERVER_DESCRIPTION").ok(),
                        }]
                    })
            }),
            security_schemes,
            security,
            tags,
            merge,
        })
    }
}

fn parse_tag_values(input: ParseStream) -> syn::Result<Vec<TagConfig>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let mut tags = Vec::new();

    while !content.is_empty() {
        tags.push(parse_tag_struct(&content)?);

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    Ok(tags)
}

fn parse_tag_struct(input: ParseStream) -> syn::Result<TagConfig> {
    let content;
    syn::braced!(content in input);

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    // Reject a repeated tag field (e.g. `name = ..., name = ...`) with a
    // spanned error instead of silently letting the later value overwrite the
    // earlier one — matches the top-level `vespera!` arg parser and
    // `parse_security_scheme_struct`.
    let mut seen_fields = HashSet::<String>::new();

    while !content.is_empty() {
        let ident: syn::Ident = content.parse()?;
        let ident_str = ident.to_string();
        if !seen_fields.insert(ident_str.clone()) {
            return Err(syn::Error::new(
                ident.span(),
                format!("duplicate tag field: `{ident_str}`"),
            ));
        }
        content.parse::<syn::Token![=]>()?;
        let value: LitStr = content.parse()?;

        match ident_str.as_str() {
            "name" => name = Some(value.value()),
            "description" => description = Some(value.value()),
            _ => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("unknown tag field: `{ident_str}`. Expected `name` or `description`"),
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
            "vespera! macro: tag configuration missing required `name` field.",
        )
    })?;

    Ok(TagConfig { name, description })
}

fn parse_security_values(input: ParseStream) -> syn::Result<Vec<String>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let entries: Punctuated<LitStr, syn::Token![,]> =
        content.parse_terminated(syn::parse::ParseBuffer::parse::<LitStr>, syn::Token![,])?;
    Ok(entries.into_iter().map(|entry| entry.value()).collect())
}

fn security_requirements(schemes: Vec<String>) -> Vec<BTreeMap<String, Vec<String>>> {
    schemes
        .into_iter()
        .map(|scheme| BTreeMap::from([(scheme, Vec::new())]))
        .collect()
}

fn parse_security_scheme_values(input: ParseStream) -> syn::Result<Vec<SecuritySchemeConfig>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let mut schemes = Vec::new();

    while !content.is_empty() {
        schemes.push(parse_security_scheme_struct(&content)?);

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    Ok(schemes)
}

fn parse_security_scheme_struct(input: ParseStream) -> syn::Result<SecuritySchemeConfig> {
    let content;
    syn::braced!(content in input);

    let mut name: Option<String> = None;
    let mut scheme_type: Option<SecuritySchemeType> = None;
    let mut description: Option<String> = None;
    let mut header_name: Option<String> = None;
    let mut location: Option<String> = None;
    let mut scheme: Option<String> = None;
    let mut bearer_format: Option<String> = None;
    let mut open_id_connect_url: Option<String> = None;
    let mut seen_fields = HashSet::<String>::new();

    while !content.is_empty() {
        let (field_name, span) = parse_security_field_name(&content)?;
        if !seen_fields.insert(field_name.clone()) {
            return Err(syn::Error::new(
                span,
                format!("duplicate security scheme field: `{field_name}`"),
            ));
        }
        content.parse::<syn::Token![=]>()?;
        let value: LitStr = content.parse()?;

        match field_name.as_str() {
            "name" => name = Some(value.value()),
            "type" => scheme_type = Some(parse_security_scheme_type(&value)?),
            "description" => description = Some(value.value()),
            "header_name" => header_name = Some(value.value()),
            "in" => location = Some(value.value()),
            "scheme" => scheme = Some(value.value()),
            "bearer_format" => bearer_format = Some(value.value()),
            "open_id_connect_url" => open_id_connect_url = Some(value.value()),
            _ => {
                return Err(syn::Error::new(
                    span,
                    format!(
                        "unknown security scheme field: `{field_name}`. Expected `name`, `type`, `description`, `header_name`, `in`, `scheme`, `bearer_format`, or `open_id_connect_url`"
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
            "vespera! macro: security scheme missing required `name` field.",
        )
    })?;
    let scheme_type = scheme_type.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "vespera! macro: security scheme missing required `type` field.",
        )
    })?;
    // Type-specific OpenAPI validity: reject an under-specified scheme at
    // compile time instead of silently emitting a spec that violates the
    // OpenAPI Security Scheme Object requirements.
    validate_security_scheme_fields(
        &name,
        scheme_type,
        location.as_deref(),
        header_name.as_deref(),
        scheme.as_deref(),
        open_id_connect_url.as_deref(),
    )?;

    Ok(SecuritySchemeConfig {
        name,
        scheme: SecurityScheme {
            r#type: scheme_type,
            description,
            name: header_name,
            r#in: location,
            scheme,
            bearer_format,
            flows: None,
            open_id_connect_url,
        },
    })
}

/// Validate that a security scheme carries the fields OpenAPI requires for
/// its `type`, so `vespera!` never emits a structurally-invalid
/// `components.securitySchemes` entry.
///
/// - `apiKey` → `header_name` (the api-key `name`) + `in` ∈ {query, header, cookie}
/// - `http` → `scheme`
/// - `openIdConnect` → `open_id_connect_url`
/// - `oauth2` → requires `flows`, which the DSL does not yet parse → rejected
///   with an explicit message (better than emitting an `oauth2` scheme with no
///   flows, which is invalid)
/// - `mutualTLS` → no extra required fields
fn validate_security_scheme_fields(
    name: &str,
    scheme_type: SecuritySchemeType,
    location: Option<&str>,
    header_name: Option<&str>,
    scheme: Option<&str>,
    open_id_connect_url: Option<&str>,
) -> syn::Result<()> {
    let span = proc_macro2::Span::call_site();
    let missing = |field: &str, hint: &str| {
        syn::Error::new(
            span,
            format!(
                "vespera! macro: security scheme `{name}` of type `{}` is missing required field `{field}` ({hint})",
                scheme_type_label(scheme_type)
            ),
        )
    };
    match scheme_type {
        SecuritySchemeType::ApiKey => {
            if header_name.is_none() {
                return Err(missing("header_name", "the api-key parameter name"));
            }
            match location {
                None => return Err(missing("in", "one of \"query\", \"header\", or \"cookie\"")),
                Some(loc) if !matches!(loc, "query" | "header" | "cookie") => {
                    return Err(syn::Error::new(
                        span,
                        format!(
                            "vespera! macro: security scheme `{name}` has invalid `in` value `{loc}`; expected \"query\", \"header\", or \"cookie\""
                        ),
                    ));
                }
                Some(_) => {}
            }
        }
        SecuritySchemeType::Http => {
            if scheme.is_none() {
                return Err(missing("scheme", "e.g. \"bearer\" or \"basic\""));
            }
        }
        SecuritySchemeType::OpenIdConnect => {
            if open_id_connect_url.is_none() {
                return Err(missing(
                    "open_id_connect_url",
                    "the OpenID Connect discovery URL",
                ));
            }
        }
        SecuritySchemeType::OAuth2 => {
            return Err(syn::Error::new(
                span,
                format!(
                    "vespera! macro: security scheme `{name}` of type `oauth2` requires `flows`, which the vespera! security_schemes DSL does not yet support"
                ),
            ));
        }
        SecuritySchemeType::MutualTls => {}
    }
    Ok(())
}

/// OpenAPI wire label for a [`SecuritySchemeType`], for diagnostics.
fn scheme_type_label(scheme_type: SecuritySchemeType) -> &'static str {
    match scheme_type {
        SecuritySchemeType::ApiKey => "apiKey",
        SecuritySchemeType::Http => "http",
        SecuritySchemeType::MutualTls => "mutualTLS",
        SecuritySchemeType::OAuth2 => "oauth2",
        SecuritySchemeType::OpenIdConnect => "openIdConnect",
    }
}

fn parse_security_field_name(input: ParseStream) -> syn::Result<(String, proc_macro2::Span)> {
    if input.peek(syn::Token![type]) {
        let token: syn::Token![type] = input.parse()?;
        Ok(("type".to_string(), token.span))
    } else if input.peek(syn::Token![in]) {
        let token: syn::Token![in] = input.parse()?;
        Ok(("in".to_string(), token.span))
    } else {
        let ident: syn::Ident = input.parse()?;
        Ok((ident.to_string(), ident.span()))
    }
}

fn parse_security_scheme_type(value: &LitStr) -> syn::Result<SecuritySchemeType> {
    match value.value().as_str() {
        "apiKey" => Ok(SecuritySchemeType::ApiKey),
        "http" => Ok(SecuritySchemeType::Http),
        "mutualTLS" => Ok(SecuritySchemeType::MutualTls),
        "oauth2" => Ok(SecuritySchemeType::OAuth2),
        "openIdConnect" => Ok(SecuritySchemeType::OpenIdConnect),
        other => Err(syn::Error::new(
            value.span(),
            format!(
                "invalid security scheme type: `{other}`. Expected `apiKey`, `http`, `mutualTLS`, `oauth2`, or `openIdConnect`"
            ),
        )),
    }
}

/// Parse merge values: merge = [`path::to::App`, `another::App`]
fn parse_merge_values(input: ParseStream) -> syn::Result<Vec<syn::Path>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let paths: Punctuated<syn::Path, syn::Token![,]> =
        content.parse_terminated(syn::Path::parse, syn::Token![,])?;
    Ok(paths.into_iter().collect())
}

fn parse_openapi_values(input: ParseStream) -> syn::Result<Vec<LitStr>> {
    input.parse::<syn::Token![=]>()?;

    if input.peek(syn::token::Bracket) {
        let content;
        let _ = bracketed!(content in input);
        let entries: Punctuated<LitStr, syn::Token![,]> =
            content.parse_terminated(syn::parse::ParseBuffer::parse::<LitStr>, syn::Token![,])?;
        Ok(entries.into_iter().collect())
    } else {
        let single: LitStr = input.parse()?;
        Ok(vec![single])
    }
}

/// Validate that a URL starts with http:// or https://
fn validate_server_url(url: &LitStr) -> syn::Result<String> {
    let url_value = url.value();
    if !url_value.starts_with("http://") && !url_value.starts_with("https://") {
        return Err(syn::Error::new(
            url.span(),
            format!(
                "invalid server URL: `{url_value}`. URL must start with `http://` or `https://`"
            ),
        ));
    }
    Ok(url_value)
}

/// Parse server values in various formats:
/// - `servers = "url"` - single URL
/// - `servers = ["url1", "url2"]` - multiple URLs (strings only)
/// - `servers = [("url", "description")]` - tuple format with descriptions
/// - `servers = [{url = "...", description = "..."}]` - struct-like format
/// - `servers = {url = "...", description = "..."}` - single server struct-like format
fn parse_servers_values(input: ParseStream) -> syn::Result<Vec<ServerConfig>> {
    use syn::token::{Brace, Paren};

    input.parse::<syn::Token![=]>()?;

    if input.peek(syn::token::Bracket) {
        // Array format: [...]
        let content;
        let _ = bracketed!(content in input);

        let mut servers = Vec::new();

        while !content.is_empty() {
            if content.peek(Paren) {
                // Parse tuple: ("url", "description")
                let tuple_content;
                syn::parenthesized!(tuple_content in content);
                let url: LitStr = tuple_content.parse()?;
                let url_value = validate_server_url(&url)?;
                let description = if tuple_content.peek(syn::Token![,]) {
                    tuple_content.parse::<syn::Token![,]>()?;
                    Some(tuple_content.parse::<LitStr>()?.value())
                } else {
                    None
                };
                servers.push(ServerConfig {
                    url: url_value,
                    description,
                });
            } else if content.peek(Brace) {
                // Parse struct-like: {url = "...", description = "..."}
                let server = parse_server_struct(&content)?;
                servers.push(server);
            } else {
                // Parse simple string: "url"
                let url: LitStr = content.parse()?;
                let url_value = validate_server_url(&url)?;
                servers.push(ServerConfig {
                    url: url_value,
                    description: None,
                });
            }

            if content.peek(syn::Token![,]) {
                content.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        Ok(servers)
    } else if input.peek(syn::token::Brace) {
        // Single struct-like format: servers = {url = "...", description = "..."}
        let server = parse_server_struct(input)?;
        Ok(vec![server])
    } else {
        // Single string: servers = "url"
        let single: LitStr = input.parse()?;
        let url_value = validate_server_url(&single)?;
        Ok(vec![ServerConfig {
            url: url_value,
            description: None,
        }])
    }
}

/// Parse a single server in struct-like format: {url = "...", description = "..."}
fn parse_server_struct(input: ParseStream) -> syn::Result<ServerConfig> {
    let content;
    syn::braced!(content in input);

    let mut url: Option<String> = None;
    let mut description: Option<String> = None;
    // Reject a repeated server field (e.g. `url = ..., url = ...`) with a
    // spanned error instead of silently letting the later value overwrite the
    // earlier one — matches the top-level `vespera!` arg parser and
    // `parse_security_scheme_struct`.
    let mut seen_fields = HashSet::<String>::new();

    while !content.is_empty() {
        let ident: syn::Ident = content.parse()?;
        let ident_str = ident.to_string();
        if !seen_fields.insert(ident_str.clone()) {
            return Err(syn::Error::new(
                ident.span(),
                format!("duplicate server field: `{ident_str}`"),
            ));
        }

        match ident_str.as_str() {
            "url" => {
                content.parse::<syn::Token![=]>()?;
                let url_lit: LitStr = content.parse()?;
                url = Some(validate_server_url(&url_lit)?);
            }
            "description" => {
                content.parse::<syn::Token![=]>()?;
                description = Some(content.parse::<LitStr>()?.value());
            }
            _ => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("unknown field: `{ident_str}`. Expected `url` or `description`"),
                ));
            }
        }

        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        } else {
            break;
        }
    }

    let url = url.ok_or_else(|| syn::Error::new(proc_macro2::Span::call_site(), "vespera! macro: server configuration missing required `url` field. Use format: `servers = { url = \"http://localhost:3000\" }` or `servers = { url = \"...\", description = \"...\" }`."))?;

    Ok(ServerConfig { url, description })
}

/// Processed vespera input with extracted values
pub struct ProcessedVesperaInput {
    pub folder_name: String,
    pub openapi_file_names: Vec<String>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub docs_url: Option<String>,
    pub redoc_url: Option<String>,
    pub servers: Option<Vec<Server>>,
    pub security_schemes: Option<BTreeMap<String, SecurityScheme>>,
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    pub tag_descriptions: Option<HashMap<String, String>>,
    /// Apps to merge (`syn::Path` for code generation)
    pub merge: Vec<syn::Path>,
}

/// Process `AutoRouterInput` into extracted values
pub fn process_vespera_input(input: AutoRouterInput) -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: input
            .dir
            .map_or_else(|| "routes".to_string(), |f| f.value()),
        openapi_file_names: input
            .openapi
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.value())
            .collect(),
        title: input.title.map(|t| t.value()),
        version: input.version.map(|v| v.value()),
        docs_url: input.docs_url.map(|u| u.value()),
        redoc_url: input.redoc_url.map(|u| u.value()),
        servers: input.servers.map(|svrs| {
            svrs.into_iter()
                .map(|s| Server {
                    url: s.url,
                    description: s.description,
                    variables: None,
                })
                .collect()
        }),
        security_schemes: input.security_schemes.and_then(|schemes| {
            let schemes = schemes
                .into_iter()
                .map(|scheme| (scheme.name, scheme.scheme))
                .collect::<BTreeMap<_, _>>();
            if schemes.is_empty() {
                None
            } else {
                Some(schemes)
            }
        }),
        security: input.security.map(security_requirements),
        tag_descriptions: input.tags.and_then(|tags| {
            let tags = tags
                .into_iter()
                .filter_map(|tag| tag.description.map(|description| (tag.name, description)))
                .collect::<HashMap<_, _>>();
            if tags.is_empty() { None } else { Some(tags) }
        }),
        merge: input.merge.unwrap_or_default(),
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
