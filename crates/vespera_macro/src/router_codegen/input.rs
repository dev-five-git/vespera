use proc_macro2::Span;
use std::collections::{BTreeMap, HashSet};
use syn::{
    LitStr, bracketed,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};
use vespera_core::openapi::Server;
use vespera_core::schema::{OAuthFlows, SecurityRequirement, SecurityScheme, SecuritySchemeType};

use super::info::{InfoArgs, is_info_field, validate_info};
use super::input_env::{arg_or_env, doc_url_or_env, env_lit};
use super::security_dsl::{parse_flows, parse_security_values, validate_top_level_scopes};

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
    /// Route serving the generated OpenAPI document as `application/json`.
    pub spec_url: Option<LitStr>,
    pub servers: Option<Vec<ServerConfig>>,
    pub security_schemes: Option<Vec<SecuritySchemeConfig>>,
    pub security: Option<Vec<SecurityRequirement>>,
    pub tags: Option<Vec<TagConfig>>,
    /// Apps to merge (e.g., [`third::ThirdApp`, `another::AnotherApp`])
    pub merge: Option<Vec<syn::Path>>,
    /// `info` object fields (`description`, `summary`, `license`, ...).
    pub info: InfoArgs,
    /// Hash of the macro arguments as written, before any environment
    /// fallback (see `vespera_impl::invocation::invocation_tag`).
    pub literal_hash: u64,
}

impl Parse for AutoRouterInput {
    #[allow(clippy::too_many_lines)]
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let literal_hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            input.cursor().token_stream().to_string().hash(&mut hasher);
            hasher.finish()
        };
        let mut dir = None;
        let mut openapi = None;
        let mut title = None;
        let mut version = None;
        let mut docs_url = None;
        let mut redoc_url = None;
        let mut spec_url = None;
        let mut servers = None;
        let mut security_schemes = None;
        let mut security = None;
        let mut tags = None;
        let mut merge = None;
        let mut info_fields = BTreeMap::<String, LitStr>::new();
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
                    "spec_url" => {
                        input.parse::<syn::Token![=]>()?;
                        spec_url = Some(input.parse()?);
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
                    field if is_info_field(field) => {
                        input.parse::<syn::Token![=]>()?;
                        info_fields.insert(ident_str.clone(), input.parse()?);
                    }
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown field: `{ident_str}`. Expected `dir`, `openapi`, `title`, `version`, `docs_url`, `redoc_url`, `spec_url`, `servers`, `security_schemes`, `security`, `tags`, `merge`, `description`, `summary`, `terms_of_service`, `contact_name`, `contact_url`, `contact_email`, `license`, `license_name`, or `license_url`"
                            ),
                        ));
                    }
                }
            } else if lookahead.peek(syn::LitStr) {
                let lit: LitStr = input.parse()?;
                if !seen_fields.insert("dir".to_string()) {
                    return Err(syn::Error::new(
                        lit.span(),
                        "duplicate field `dir` in vespera! macro",
                    ));
                }
                // If just a string, treat it as dir (for backward compatibility)
                dir = Some(lit);
            } else {
                return Err(lookahead.error());
            }

            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            } else {
                break;
            }
        }

        let (docs_url, docs_label) = doc_url_or_env(docs_url, "docs_url", "VESPERA_DOCS_URL")?;
        let (redoc_url, redoc_label) = doc_url_or_env(redoc_url, "redoc_url", "VESPERA_REDOC_URL")?;
        let (spec_url, spec_label) = doc_url_or_env(spec_url, "spec_url", "VESPERA_SPEC_URL")?;
        validate_doc_urls(&[
            (docs_label.as_str(), docs_url.as_ref()),
            (redoc_label.as_str(), redoc_url.as_ref()),
            (spec_label.as_str(), spec_url.as_ref()),
        ])?;
        let info = validate_info(&info_fields)?;
        let security = match security {
            Some((requirements, arr)) => {
                validate_top_level_scopes(&requirements, &arr, security_schemes.as_deref())?;
                Some(requirements)
            }
            None => None,
        };

        let dir_from_env = dir.is_none();
        let dir = arg_or_env(dir, "VESPERA_DIR")?;
        if let Some(dir) = &dir {
            super::validate_route_dir(&dir.value(), "vespera!", dir.span()).map_err(|err| {
                if dir_from_env {
                    crate::env_access::env_error(&err, "VESPERA_DIR")
                } else {
                    err
                }
            })?;
        }

        Ok(Self {
            literal_hash,
            dir,
            openapi: match openapi {
                Some(files) => Some(files),
                None => env_lit("VESPERA_OPENAPI")?.map(|f| vec![f]),
            },
            title: arg_or_env(title, "VESPERA_TITLE")?,
            version: arg_or_env(version, "VESPERA_VERSION")?.or_else(|| {
                crate::env_access::var("CARGO_PKG_VERSION")
                    .map(|f| LitStr::new(&f, Span::call_site()))
                    .ok()
            }),
            docs_url,
            redoc_url,
            spec_url,
            servers: match servers {
                Some(servers) => Some(servers),
                None => env_server()?,
            },
            security_schemes,
            security,
            tags,
            merge,
            info,
        })
    }
}

/// The `VESPERA_SERVER_URL` (+ `VESPERA_SERVER_DESCRIPTION`) fallback
/// server, validated like a `servers = ...` argument: an invalid value is a
/// compile error naming the variable, never silently dropped.
///
/// `VESPERA_SERVER_DESCRIPTION` is read only together with a URL: without
/// `VESPERA_SERVER_URL` it is ignored (no server is emitted, no error).
fn env_server() -> syn::Result<Option<Vec<ServerConfig>>> {
    let Some(url) = env_lit("VESPERA_SERVER_URL")? else {
        return Ok(None);
    };
    let url = validate_server_url(&url)
        .map_err(|err| crate::env_access::env_error(&err, "VESPERA_SERVER_URL"))?;
    let description = crate::env_access::non_empty_var("VESPERA_SERVER_DESCRIPTION")?;
    Ok(Some(vec![ServerConfig { url, description }]))
}

/// Reject `docs_url` / `redoc_url` / `spec_url` values axum would panic on at
/// router build time: a path not starting with `/`, a segment starting with
/// axum 0.7 parameter syntax (`:` / `*`), or two of them on the same path.
/// `{` / `}` (axum 0.8 parameters) are rejected too: a documentation route is
/// a static path.
fn validate_doc_urls(urls: &[(&str, Option<&LitStr>)]) -> syn::Result<()> {
    let mut seen: Vec<(&str, String)> = Vec::new();
    for (key, lit) in urls {
        let Some(lit) = lit else { continue };
        let value = lit.value();
        if !value.starts_with('/') {
            return Err(syn::Error::new(
                lit.span(),
                format!("vespera! macro: {key} must start with `/`, got \"{value}\""),
            ));
        }
        // A request's matched path never has a query or fragment, and a space
        // arrives percent-encoded: such a route could never be reached.
        if value.contains(['?', '#']) || value.chars().any(char::is_whitespace) {
            return Err(syn::Error::new(
                lit.span(),
                format!(
                    "vespera! macro: {key} must be a path without a query, a fragment or whitespace, got \"{value}\""
                ),
            ));
        }
        // A client requests `/api/../docs` as `/docs` (and `%2e%2e`, `\` alike):
        // the page would be reachable only at another URL than the one named.
        if super::path_segments::resolved_by_clients(&value) {
            return Err(syn::Error::new(
                lit.span(),
                format!(
                    "vespera! macro: {key} must not contain `.` or `..` path segments (also percent-encoded) or a backslash, got \"{value}\""
                ),
            ));
        }
        if value.contains(['{', '}'])
            || value
                .split('/')
                .any(|segment| segment.starts_with([':', '*']))
        {
            return Err(syn::Error::new(
                lit.span(),
                format!(
                    "vespera! macro: {key} must be a static path, got \"{value}\": `:`, `*`, `{{` and `}}` are axum route-parameter syntax"
                ),
            ));
        }
        // A request-target is ASCII: a raw non-ASCII character never matches
        // the percent-encoded bytes a client sends.
        if !super::path_segments::is_ascii_path(&value) {
            return Err(syn::Error::new(
                lit.span(),
                format!(
                    "vespera! macro: {key} must be ASCII URL path characters (percent-encode any other byte), got \"{value}\""
                ),
            ));
        }
        if let Some((other, _)) = seen.iter().find(|(_, v)| *v == value) {
            return Err(syn::Error::new(
                lit.span(),
                format!(
                    "vespera! macro: {key} and {other} both use \"{value}\"; each documentation route needs its own path"
                ),
            ));
        }
        seen.push((key, value));
    }
    Ok(())
}

fn parse_tag_values(input: ParseStream) -> syn::Result<Vec<TagConfig>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let mut tags = Vec::new();

    while !content.is_empty() {
        let span = content.span();
        let tag = parse_tag_struct(&content)?;
        if tags.iter().any(|seen: &TagConfig| seen.name == tag.name) {
            return Err(syn::Error::new(
                span,
                format!("duplicate tag name `{}` in `tags`", tag.name),
            ));
        }
        tags.push(tag);

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

fn parse_security_scheme_values(input: ParseStream) -> syn::Result<Vec<SecuritySchemeConfig>> {
    input.parse::<syn::Token![=]>()?;

    let content;
    let _ = bracketed!(content in input);
    let mut schemes = Vec::new();

    while !content.is_empty() {
        let span = content.span();
        let scheme = parse_security_scheme_struct(&content)?;
        if schemes
            .iter()
            .any(|seen: &SecuritySchemeConfig| seen.name == scheme.name)
        {
            return Err(syn::Error::new(
                span,
                format!(
                    "duplicate security scheme name `{}` in `security_schemes`",
                    scheme.name
                ),
            ));
        }
        schemes.push(scheme);

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
    let brace = syn::braced!(content in input);
    // Errors about the scheme as a whole point at its `{ ... }`.
    let span = brace.span.join();

    let mut name: Option<String> = None;
    let mut scheme_type: Option<SecuritySchemeType> = None;
    let mut description: Option<String> = None;
    let mut header_name: Option<String> = None;
    let mut location: Option<String> = None;
    let mut scheme: Option<String> = None;
    let mut bearer_format: Option<String> = None;
    let mut open_id_connect_url: Option<String> = None;
    let mut flows: Option<OAuthFlows> = None;
    let mut seen_fields = HashSet::<String>::new();

    while !content.is_empty() {
        let (field_name, span) = parse_security_field_name(&content)?;
        if !seen_fields.insert(field_name.clone()) {
            return Err(syn::Error::new(
                span,
                format!("duplicate security scheme field: `{field_name}`"),
            ));
        }
        if field_name == "flows" {
            flows = Some(parse_flows(&content)?);
            if !content.peek(syn::Token![,]) {
                break;
            }
            content.parse::<syn::Token![,]>()?;
            continue;
        }
        content.parse::<syn::Token![=]>()?;
        let value: LitStr = content.parse()?;

        match field_name.as_str() {
            "name" => {
                super::security_dsl::require_component_key(&value)?;
                name = Some(value.value());
            }
            "type" => scheme_type = Some(parse_security_scheme_type(&value)?),
            "description" => description = Some(value.value()),
            "header_name" => {
                super::security_dsl::require_non_empty("header_name", &value)?;
                header_name = Some(value.value());
            }
            "in" => location = Some(value.value()),
            "scheme" => {
                super::security_dsl::require_non_empty("scheme", &value)?;
                scheme = Some(value.value());
            }
            "bearer_format" => {
                super::security_dsl::require_non_empty("bearer_format", &value)?;
                bearer_format = Some(value.value());
            }
            "open_id_connect_url" => {
                super::security_dsl::require_absolute_url("open_id_connect_url", &value)?;
                open_id_connect_url = Some(value.value());
            }
            _ => {
                return Err(syn::Error::new(
                    span,
                    format!(
                        "unknown security scheme field: `{field_name}`. Expected `name`, `type`, `description`, `header_name`, `in`, `scheme`, `bearer_format`, `open_id_connect_url`, or `flows`"
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
            span,
            "vespera! macro: security scheme missing required `name` field.",
        )
    })?;
    let scheme_type = scheme_type.ok_or_else(|| {
        syn::Error::new(
            span,
            "vespera! macro: security scheme missing required `type` field.",
        )
    })?;
    let scheme = SecurityScheme {
        r#type: scheme_type,
        description,
        name: header_name,
        r#in: location,
        scheme,
        bearer_format,
        flows,
        open_id_connect_url,
    };
    // Type-specific OpenAPI validity: reject an under-specified scheme, or a
    // field its type does not take, at compile time instead of silently
    // emitting a spec that violates the OpenAPI Security Scheme Object.
    validate_security_scheme_fields(&name, &scheme, span)?;
    Ok(SecuritySchemeConfig { name, scheme })
}

/// Validate that a security scheme carries exactly the fields OpenAPI
/// defines for its `type`, so `vespera!` never emits a structurally-invalid
/// `components.securitySchemes` entry (the OpenAPI 3.1 schema evaluates them
/// per type with `unevaluatedProperties: false`).
///
/// - `apiKey` → `header_name` (the api-key `name`) + `in` ∈ {query, header, cookie}
/// - `http` → `scheme`; `bearer_format` only with `scheme = "bearer"`
/// - `openIdConnect` → `open_id_connect_url`
/// - `oauth2` → `flows` (at least one flow — checked by [`parse_flows`])
/// - `mutualTLS` → no extra required fields
/// - any of those fields on another type → rejected
fn validate_security_scheme_fields(
    name: &str,
    scheme: &SecurityScheme,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    let label = scheme_type_label(scheme.r#type);
    let missing = |field: &str, hint: &str| {
        syn::Error::new(
            span,
            format!(
                "vespera! macro: security scheme `{name}` of type `{label}` is missing required field `{field}` ({hint})"
            ),
        )
    };
    // `(field, set, the types OpenAPI defines it for)`.
    let typed_fields = [
        ("header_name", scheme.name.is_some(), "apiKey"),
        ("in", scheme.r#in.is_some(), "apiKey"),
        ("scheme", scheme.scheme.is_some(), "http"),
        ("bearer_format", scheme.bearer_format.is_some(), "http"),
        (
            "open_id_connect_url",
            scheme.open_id_connect_url.is_some(),
            "openIdConnect",
        ),
        ("flows", scheme.flows.is_some(), "oauth2"),
    ];
    if let Some((field, _, owner)) = typed_fields
        .iter()
        .find(|(_, set, owner)| *set && *owner != label)
    {
        return Err(syn::Error::new(
            span,
            format!(
                "vespera! macro: security scheme `{name}` of type `{label}` cannot declare `{field}`; only `{owner}` schemes have {field}"
            ),
        ));
    }
    match scheme.r#type {
        SecuritySchemeType::ApiKey => {
            if scheme.name.is_none() {
                return Err(missing("header_name", "the api-key parameter name"));
            }
            match scheme.r#in.as_deref() {
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
        SecuritySchemeType::Http => match scheme.scheme.as_deref() {
            None => return Err(missing("scheme", "e.g. \"bearer\" or \"basic\"")),
            Some(http)
                if scheme.bearer_format.is_some() && !http.eq_ignore_ascii_case("bearer") =>
            {
                return Err(syn::Error::new(
                    span,
                    format!(
                        "vespera! macro: security scheme `{name}` declares `bearer_format` with `scheme = \"{http}\"`; OpenAPI defines `bearerFormat` only for `scheme = \"bearer\"`"
                    ),
                ));
            }
            Some(_) => {}
        },
        SecuritySchemeType::OpenIdConnect => {
            if scheme.open_id_connect_url.is_none() {
                return Err(missing(
                    "open_id_connect_url",
                    "the OpenID Connect discovery URL",
                ));
            }
        }
        SecuritySchemeType::OAuth2 => {
            if scheme.flows.is_none() {
                return Err(missing(
                    "flows",
                    "e.g. `flows = { client_credentials = { token_url = \"...\", scopes = { ... } } }`",
                ));
            }
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

/// Whether `url` is a server URL `vespera!` accepts: an absolute `http(s)`
/// URL (the same check as the `info` URLs: a host, no whitespace) or a
/// relative reference to the document's host (`/api/v1`, which OpenAPI 3.1
/// resolves against the URL serving the document).  `//host/..` is a
/// network-path reference: it names another host, so it is not accepted
/// either.  A relative reference carries no whitespace, query or fragment
/// (the same bar as the absolute form, plus `?` / `#`, which an OpenAPI server
/// URL does not carry).  A backslash is refused too: WHATWG URL parsing reads
/// `\` as `/` for http(s), so `/\host` is the same network-path reference as
/// `//host`.  The absolute form keeps the `info` URL rule, which allows a
/// query or fragment; only the relative form refuses them.  `{var}` templates
/// are not accepted: `vespera!` cannot declare the `variables` they need.
fn is_server_url(url: &str) -> bool {
    let relative = url.starts_with('/')
        && !url.starts_with("//")
        && !url.contains(char::is_whitespace)
        && !url.contains(['?', '#', '\\']);
    (super::info::is_absolute_http_url(url) || relative) && !is_url_template(url)
}

/// Whether `url` holds a `{var}` server-variable template.
fn is_url_template(url: &str) -> bool {
    url.contains(['{', '}'])
}

/// Validate a server URL (see [`is_server_url`]).
fn validate_server_url(url: &LitStr) -> syn::Result<String> {
    let url_value = url.value();
    if is_url_template(&url_value) {
        return Err(syn::Error::new(
            url.span(),
            format!(
                "invalid server URL: `{url_value}`. `{{var}}` templates are not supported: `vespera!` cannot declare the server `variables` they need"
            ),
        ));
    }
    if !is_server_url(&url_value) {
        return Err(syn::Error::new(
            url.span(),
            format!(
                "invalid server URL: `{url_value}`. URL must start with `http://`, `https://` or `/` (a URL relative to the document's host)"
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
#[derive(Default)]
pub struct ProcessedVesperaInput {
    pub folder_name: String,
    pub openapi_file_names: Vec<String>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub docs_url: Option<String>,
    pub redoc_url: Option<String>,
    pub spec_url: Option<String>,
    pub servers: Option<Vec<Server>>,
    pub security_schemes: Option<BTreeMap<String, SecurityScheme>>,
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    /// Declared `tags = [...]` in declaration order (`(name, description)`).
    pub tag_descriptions: Option<Vec<(String, Option<String>)>>,
    /// Apps to merge (`syn::Path` for code generation)
    pub merge: Vec<syn::Path>,
    /// `info` object fields beyond `title` / `version`.
    pub info: InfoArgs,
    /// [`AutoRouterInput::literal_hash`] (`None` for an input built in code).
    pub literal_hash: Option<u64>,
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
        spec_url: input.spec_url.map(|u| u.value()),
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
        security: input.security,
        tag_descriptions: input.tags.filter(|tags| !tags.is_empty()).map(|tags| {
            tags.into_iter()
                .map(|tag| (tag.name, tag.description))
                .collect()
        }),
        merge: input.merge.unwrap_or_default(),
        info: input.info,
        literal_hash: Some(input.literal_hash),
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "input_tests_server_url.rs"]
mod server_url_tests;

#[cfg(test)]
#[path = "input_tests_doc_urls.rs"]
mod doc_url_tests;

#[cfg(test)]
#[path = "input_tests_security.rs"]
mod security_tests;

#[cfg(test)]
#[path = "input_tests_env.rs"]
mod env_tests;
