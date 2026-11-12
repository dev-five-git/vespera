use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::{
    route::{Parameter, ParameterLocation},
    schema::{Schema, SchemaRef},
};

use super::shared::is_primitive_or_like;
use crate::parser::schema::parse_type_to_schema_ref;

pub(super) fn parse_option_typed_header(param_name: &str, ty: &Type) -> Option<Vec<Parameter>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.first()?;
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
    typed_header_parameter(param_name, inner_segment, false).map(|param| vec![param])
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
    match segment.ident.to_string().as_str() {
        "Header" => parse_header(param_name, segment, known_schemas, struct_definitions),
        "TypedHeader" => typed_header_parameter(param_name, segment, true).map(|param| vec![param]),
        _ => None,
    }
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

/// The header parameter for `TypedHeader<T>`: named after the HEADER when `T`
/// is a known `headers::*` type, otherwise after the Rust binding
/// (`_` → `-`). Returns `None` for headers that
/// OpenAPI forbids as header parameters (`Authorization`, `Accept`,
/// `Content-Type`).
fn typed_header_parameter(
    param_name: &str,
    typed_header: &syn::PathSegment,
    required: bool,
) -> Option<Parameter> {
    // `TypedHeader<T>` has exactly one (type) parameter.
    let header_ident = if let syn::PathArguments::AngleBracketed(args) = &typed_header.arguments
        && let Some(syn::GenericArgument::Type(Type::Path(path))) = args.args.first()
    {
        path.path.segments.last().map(|seg| seg.ident.to_string())
    } else {
        None
    };
    // Only the well-known `headers::*` types have a wire name we can trust.
    // A custom `headers::Header` impl declares its name in `Header::name()`,
    // which a macro cannot see — fall back to the binding name (the pre-0.x
    // behaviour) instead of guessing from the type name.
    let name = header_ident
        .as_deref()
        .and_then(known_typed_header_name)
        .map_or_else(|| param_name.replace('_', "-"), str::to_string);
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
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn malformed_option_and_header_generics_are_rejected() {
        let bare_option: Type = syn::parse_quote!(Option);
        assert!(parse_option_typed_header("x", &bare_option).is_none());
        let lifetime_option: Type = syn::parse_quote!(Option<'static>);
        assert!(parse_option_typed_header("x", &lifetime_option).is_none());
        // An `Option` of anything but `TypedHeader` is left to the other
        // extractor parsers.
        let plain_option: Type = syn::parse_quote!(Option<String>);
        assert!(parse_option_typed_header("x", &plain_option).is_none());

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

    /// Known `headers::*` types are named after the header; custom types fall
    /// back to the Rust binding name; OpenAPI-reserved names
    /// (`Authorization`/`Accept`/`Content-Type`) are not emitted.
    #[rstest]
    #[case::user_agent("TypedHeader<UserAgent>", Some("User-Agent"))]
    #[case::qualified("TypedHeader<headers::IfNoneMatch>", Some("If-None-Match"))]
    #[case::etag("TypedHeader<ETag>", Some("ETag"))]
    #[case::te("TypedHeader<Te>", Some("TE"))]
    #[case::custom("TypedHeader<XRequestId>", Some("my-binding"))]
    #[case::custom_qualified("TypedHeader<my_headers::TenantId>", Some("my-binding"))]
    #[case::cookie("TypedHeader<Cookie>", Some("Cookie"))]
    #[case::authorization("TypedHeader<Authorization<Bearer>>", None)]
    #[case::content_type("TypedHeader<ContentType>", None)]
    #[case::accept("TypedHeader<Accept>", None)]
    #[case::bare("TypedHeader", Some("my-binding"))]
    fn typed_header_names(#[case] ty_src: &str, #[case] expected: Option<&str>) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        let required = parse_header_extractor("my_binding", &ty, &HashSet::new(), &HashMap::new())
            .map(|params| params[0].name.clone());
        assert_eq!(required.as_deref(), expected, "{ty_src}");

        let optional: Type = syn::parse_str(&format!("Option<{ty_src}>")).unwrap();
        let optional = parse_option_typed_header("my_binding", &optional);
        assert_eq!(
            optional.as_ref().map(|params| params[0].name.as_str()),
            expected
        );
        if let Some(params) = optional {
            assert_eq!(params[0].required, Some(false));
        }
    }
}
