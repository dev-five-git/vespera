use std::collections::{BTreeMap, HashSet};

use syn::{FnArg, PatType, Type};
use vespera_core::route::{MediaType, RequestBody};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::{extractors::unwrap_validated_type, schema::parse_type_to_schema_ref};

fn is_string_like(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "String" || seg.ident == "str"),
        Type::Reference(type_ref) => is_string_like(&type_ref.elem),
        _ => false,
    }
}

/// Generic single-type body extractors: `(extractor ident, media type)`.
///
/// Each entry maps `Extractor<T>` to a request body whose only content entry is
/// `media type` holding `T`'s schema.
const GENERIC_BODY_EXTRACTORS: [(&str, &str); 3] = [
    ("Json", "application/json"),
    ("Form", "application/x-www-form-urlencoded"),
    ("TypedMultipart", "multipart/form-data"),
];

/// Build a required `RequestBody` carrying exactly one media type entry.
fn single_content_body(media_type: &str, schema: SchemaRef) -> RequestBody {
    let mut content = BTreeMap::new();
    content.insert(
        media_type.to_string(),
        MediaType {
            schema: Some(schema),
            example: None,
            examples: None,
        },
    );
    RequestBody {
        description: None,
        required: Some(true),
        content,
    }
}

/// First angle-bracketed generic argument of `segment`, if it is a type.
fn first_generic_type(segment: &syn::PathSegment) -> Option<&Type> {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    match args.args.first() {
        Some(syn::GenericArgument::Type(inner_ty)) => Some(inner_ty),
        _ => None,
    }
}

/// Analyze function signature and extract `RequestBody`
pub fn parse_request_body(
    arg: &FnArg,
    known_schemas: &HashSet<&str>,
    struct_definitions: &std::collections::HashMap<&str, &str>,
) -> Option<RequestBody> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { ty, .. }) => {
            let ty = unwrap_validated_type(ty.as_ref());
            if let Type::Path(type_path) = ty {
                // Check the last segment (handles both Json<T> and vespera::axum::Json<T>)
                let segment = type_path.path.segments.last()?;
                let ident = &segment.ident;

                // Json<T> / Form<T> / TypedMultipart<T> → single media-type request body
                if let Some(media_type) = GENERIC_BODY_EXTRACTORS
                    .iter()
                    .find_map(|(name, media_type)| (ident == name).then_some(*media_type))
                    && let Some(inner_ty) = first_generic_type(segment)
                {
                    let schema =
                        parse_type_to_schema_ref(inner_ty, known_schemas, struct_definitions);
                    return Some(single_content_body(media_type, schema));
                }

                // Raw Multipart extractor (untyped) → multipart/form-data with generic object schema
                if ident == "Multipart" && matches!(segment.arguments, syn::PathArguments::None) {
                    return Some(single_content_body(
                        "multipart/form-data",
                        SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))),
                    ));
                }
            }

            if is_string_like(ty) {
                let schema = parse_type_to_schema_ref(ty, known_schemas, struct_definitions);
                return Some(single_content_body("text/plain", schema));
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("String", true)]
    #[case("str", true)]
    #[case("&String", true)]
    #[case("&str", true)]
    #[case("i32", false)]
    #[case("Vec<String>", false)]
    #[case("!", false)]
    fn test_is_string_like_cases(#[case] ty_src: &str, #[case] expected: bool) {
        let ty: Type = syn::parse_str(ty_src).expect("type parse failed");
        assert_eq!(is_string_like(&ty), expected);
    }

    #[rstest]
    #[case::json("fn test(Json(payload): Json<User>) {}", true, "json")]
    #[case::validated_json(
        "fn test(Validated(Json(payload)): Validated<Json<User>>) {}",
        true,
        "validated_json"
    )]
    #[case::validated_form(
        "fn test(Validated(Form(input)): Validated<Form<User>>) {}",
        true,
        "validated_form"
    )]
    #[case::form("fn test(Form(input): Form<User>) {}", true, "form")]
    #[case::string("fn test(just_string: String) {}", true, "string")]
    #[case::str("fn test(just_str: &str) {}", true, "str")]
    #[case::i32("fn test(just_i32: i32) {}", false, "i32")]
    #[case::vec_string("fn test(just_vec_string: Vec<String>) {}", false, "vec_string")]
    #[case::typed_multipart(
        "fn test(TypedMultipart(req): TypedMultipart<UploadRequest>) {}",
        true,
        "typed_multipart"
    )]
    #[case::multipart_raw("fn test(multipart: Multipart) {}", true, "multipart_raw")]
    #[case::self_ref("fn test(&self) {}", false, "self_ref")]
    fn test_parse_request_body_cases(
        #[case] func_src: &str,
        #[case] has_body: bool,
        #[case] suffix: &str,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        let arg = func.sig.inputs.first().unwrap();
        let body = parse_request_body(arg, &HashSet::new(), &HashMap::new());
        assert_eq!(body.is_some(), has_body);
        with_settings!({ snapshot_suffix => format!("req_body_{}", suffix) }, {
            assert_debug_snapshot!(body);
        });
    }
}
