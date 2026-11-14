use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::{Schema, SchemaRef},
};

use super::shared::is_primitive_or_like;
use crate::parser::schema::parse_type_to_schema_ref;

/// `TypedHeader<T>` (required) or `Option<TypedHeader<T>>` (optional). The
/// header is named after `T`, so the argument pattern (`TypedHeader(_)`,
/// `_`, a binding) never matters.
pub(super) fn parse_typed_header(ty: &Type) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident == "TypedHeader" {
        return typed_header_parameter(segment, true).map(|param| vec![param]);
    }
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(Type::Path(inner_type_path))) = args.args.first() else {
        return None;
    };
    let inner_segment = inner_type_path.path.segments.last()?;
    if inner_segment.ident != "TypedHeader" {
        return None;
    }
    typed_header_parameter(inner_segment, false).map(|param| vec![param])
}

pub(super) fn parse_header_extractor(
    param_name: &str,
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Header" {
        return None;
    }
    parse_header(param_name, segment, known_schemas, struct_definitions)
}

fn parse_header(
    param_name: &str,
    segment: &syn::PathSegment,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<Vec<Parameter>> {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    if is_primitive_or_like(inner_ty) {
        return None;
    }
    Some(vec![Parameter {
        name: param_name.to_string(),
        r#in: ParameterLocation::Header,
        description: None,
        required: Some(true),
        schema: Some(parse_type_to_schema_ref(
            inner_ty,
            known_schemas,
            struct_definitions,
        )),
        example: None,
        style: None,
        explode: None,
    }])
}

/// Wire names of the well-known `headers::*` typed headers.  Any other type is
/// treated as a custom header and named after its Rust binding instead.
fn known_typed_header_name(ident: &str) -> Option<&'static str> {
    Some(match ident {
        "Authorization" => "Authorization",
        "ContentType" => "Content-Type",
        "Accept" => "Accept",
        "UserAgent" => "User-Agent",
        "Cookie" => "Cookie",
        "Host" => "Host",
        "Origin" => "Origin",
        "Referer" => "Referer",
        "IfNoneMatch" => "If-None-Match",
        "IfMatch" => "If-Match",
        "IfModifiedSince" => "If-Modified-Since",
        "IfUnmodifiedSince" => "If-Unmodified-Since",
        "IfRange" => "If-Range",
        "ContentLength" => "Content-Length",
        "ContentEncoding" => "Content-Encoding",
        "ContentRange" => "Content-Range",
        "ContentLocation" => "Content-Location",
        "ContentDisposition" => "Content-Disposition",
        "CacheControl" => "Cache-Control",
        "Connection" => "Connection",
        "Date" => "Date",
        "ETag" => "ETag",
        "Expect" => "Expect",
        "Pragma" => "Pragma",
        "ProxyAuthorization" => "Proxy-Authorization",
        "Range" => "Range",
        "Te" => "TE",
        "TransferEncoding" => "Transfer-Encoding",
        "Upgrade" => "Upgrade",
        "AccessControlRequestMethod" => "Access-Control-Request-Method",
        "AccessControlRequestHeaders" => "Access-Control-Request-Headers",
        "SecWebsocketKey" => "Sec-WebSocket-Key",
        "SecWebsocketVersion" => "Sec-WebSocket-Version",
        _ => return None,
    })
}

/// Header names OpenAPI says a header parameter MUST NOT use: they are
/// described by `securitySchemes` (`Authorization`) or by the request /
/// response media types (`Accept`, `Content-Type`), and tooling ignores them.
fn is_reserved_header_name(name: &str) -> bool {
    ["accept", "content-type", "authorization"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

/// `XRequestId` → `x-request-id`, `HTTPVersion` → `http-version`,
/// `Api_Key` → `api-key`.
fn kebab_case_type_name(ident: &str) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let mut out = String::with_capacity(ident.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            out.push('-');
            continue;
        }
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_is_lower)
            {
                out.push('-');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// The header parameter for `TypedHeader<T>`: named after the HEADER when `T`
/// is a known `headers::*` type, otherwise after `T` in kebab-case
/// (`XRequestId` → `x-request-id`). Returns `None` for headers that
/// OpenAPI forbids as header parameters (`Authorization`, `Accept`,
/// `Content-Type`).
///
/// Limitation: a custom `headers::Header` impl declares its wire name in
/// `Header::name()`, which a macro cannot see. The kebab-cased type name is
/// a convention, not the truth — name the type after its header
/// (`struct XRequestId` for `x-request-id`) to keep the document right.
fn typed_header_parameter(typed_header: &syn::PathSegment, required: bool) -> Option<Parameter> {
    // `TypedHeader<T>` has exactly one (type) parameter.
    let header_ident = if let syn::PathArguments::AngleBracketed(args) = &typed_header.arguments
        && let Some(syn::GenericArgument::Type(Type::Path(path))) = args.args.first()
    {
        path.path.segments.last().map(|seg| seg.ident.to_string())
    } else {
        None
    };
    let header_ident = header_ident?;
    let name = known_typed_header_name(&header_ident)
        .map_or_else(|| kebab_case_type_name(&header_ident), str::to_string);
    if is_reserved_header_name(&name) {
        return None;
    }
    Some(Parameter {
        name,
        r#in: ParameterLocation::Header,
        description: None,
        required: Some(required),
        schema: Some(SchemaRef::Inline(Box::new(Schema::string()))),
        example: None,
        style: None,
        explode: None,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn malformed_option_and_header_generics_are_rejected() {
        let bare_option: Type = syn::parse_quote!(Option);
        assert!(parse_typed_header(&bare_option).is_none());
        let lifetime_option: Type = syn::parse_quote!(Option<'static>);
        assert!(parse_typed_header(&lifetime_option).is_none());
        // An `Option` of anything but `TypedHeader` is left to the other
        // extractor parsers.
        let plain_option: Type = syn::parse_quote!(Option<String>);
        assert!(parse_typed_header(&plain_option).is_none());
        let reference: Type = syn::parse_quote!(&str);
        assert!(parse_typed_header(&reference).is_none());
        let other: Type = syn::parse_quote!(Vec<TypedHeader<XTenant>>);
        assert!(parse_typed_header(&other).is_none());
        let typed: Type = syn::parse_quote!(TypedHeader<XTenant>);
        assert!(parse_header_extractor("x", &typed, &HashSet::new(), &HashMap::new()).is_none());

        let bare_header: Type = syn::parse_quote!(Header);
        assert!(
            parse_header_extractor("x", &bare_header, &HashSet::new(), &HashMap::new()).is_none()
        );
        let lifetime_header: Type = syn::parse_quote!(Header<'static>);
        assert!(
            parse_header_extractor("x", &lifetime_header, &HashSet::new(), &HashMap::new())
                .is_none()
        );
    }

    /// Known `headers::*` types are named after the header; custom types are
    /// named after the type in kebab-case (never after the Rust binding);
    /// OpenAPI-reserved names (`Authorization`/`Accept`/`Content-Type`)
    /// are not emitted.
    #[rstest]
    #[case::user_agent("TypedHeader<UserAgent>", Some("User-Agent"))]
    #[case::qualified("TypedHeader<headers::IfNoneMatch>", Some("If-None-Match"))]
    #[case::etag("TypedHeader<ETag>", Some("ETag"))]
    #[case::te("TypedHeader<Te>", Some("TE"))]
    #[case::custom("TypedHeader<XRequestId>", Some("x-request-id"))]
    #[case::custom_qualified("TypedHeader<my_headers::TenantId>", Some("tenant-id"))]
    #[case::custom_acronym("TypedHeader<HTTPVersion2Hint>", Some("http-version2-hint"))]
    #[case::custom_snake("TypedHeader<Api_Key>", Some("api-key"))]
    #[case::cookie("TypedHeader<Cookie>", Some("Cookie"))]
    #[case::authorization("TypedHeader<Authorization<Bearer>>", None)]
    #[case::content_type("TypedHeader<ContentType>", None)]
    #[case::accept("TypedHeader<Accept>", None)]
    #[case::bare("TypedHeader", None)]
    fn typed_header_names(#[case] ty_src: &str, #[case] expected: Option<&str>) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        let required = parse_typed_header(&ty).map(|params| params[0].name.clone());
        assert_eq!(required.as_deref(), expected, "{ty_src}");

        let optional: Type = syn::parse_str(&format!("Option<{ty_src}>")).unwrap();
        let optional = parse_typed_header(&optional);
        assert_eq!(
            optional.as_ref().map(|params| params[0].name.as_str()),
            expected
        );
        if let Some(params) = optional {
            assert_eq!(params[0].required, Some(false));
        }
    }

    /// The header name never depends on the pattern: `TypedHeader(_)`,
    /// `_` and a destructured binding name the same header.
    #[rstest]
    #[case::wildcard_inner("fn f(TypedHeader(_): TypedHeader<UserAgent>)", "User-Agent", true)]
    #[case::wildcard("fn f(_: TypedHeader<XTenant>)", "x-tenant", true)]
    #[case::binding("fn f(TypedHeader(h): TypedHeader<XTenant>)", "x-tenant", true)]
    #[case::optional_wildcard("fn f(_: Option<TypedHeader<XTenant>>)", "x-tenant", false)]
    fn typed_header_name_ignores_the_pattern(
        #[case] sig: &str,
        #[case] name: &str,
        #[case] required: bool,
    ) {
        let sig: syn::Signature = syn::parse_str(sig).unwrap();
        let params = crate::parser::parameters::parse_function_parameter(
            &sig.inputs[0],
            &[],
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        )
        .expect("header parameter");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, name);
        assert_eq!(params[0].required, Some(required));
    }

    /// Every well-known `headers::*` type maps to its exact wire name.
    #[test]
    fn known_typed_header_names_cover_the_whole_table() {
        let table = [
            ("Authorization", "Authorization"),
            ("ContentType", "Content-Type"),
            ("Accept", "Accept"),
            ("UserAgent", "User-Agent"),
            ("Cookie", "Cookie"),
            ("Host", "Host"),
            ("Origin", "Origin"),
            ("Referer", "Referer"),
            ("IfNoneMatch", "If-None-Match"),
            ("IfMatch", "If-Match"),
            ("IfModifiedSince", "If-Modified-Since"),
            ("IfUnmodifiedSince", "If-Unmodified-Since"),
            ("IfRange", "If-Range"),
            ("ContentLength", "Content-Length"),
            ("ContentEncoding", "Content-Encoding"),
            ("ContentRange", "Content-Range"),
            ("ContentLocation", "Content-Location"),
            ("ContentDisposition", "Content-Disposition"),
            ("CacheControl", "Cache-Control"),
            ("Connection", "Connection"),
            ("Date", "Date"),
            ("ETag", "ETag"),
            ("Expect", "Expect"),
            ("Pragma", "Pragma"),
            ("ProxyAuthorization", "Proxy-Authorization"),
            ("Range", "Range"),
            ("Te", "TE"),
            ("TransferEncoding", "Transfer-Encoding"),
            ("Upgrade", "Upgrade"),
            (
                "AccessControlRequestMethod",
                "Access-Control-Request-Method",
            ),
            (
                "AccessControlRequestHeaders",
                "Access-Control-Request-Headers",
            ),
            ("SecWebsocketKey", "Sec-WebSocket-Key"),
            ("SecWebsocketVersion", "Sec-WebSocket-Version"),
        ];
        for (ident, wire) in table {
            assert_eq!(known_typed_header_name(ident), Some(wire), "{ident}");
        }
        assert_eq!(known_typed_header_name("XRequestId"), None);
    }
}
