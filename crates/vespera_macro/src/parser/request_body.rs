use std::collections::BTreeMap;

use syn::{FnArg, PatType, Type};
use vespera_core::route::{MediaType, RequestBody};

use super::schema::parse_type_to_schema_ref_with_schemas;

fn is_string_like(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident == "String" || seg.ident == "str")
            .unwrap_or(false),
        Type::Reference(type_ref) => is_string_like(&type_ref.elem),
        _ => false,
    }
}

/// Analyze function signature and extract RequestBody
pub fn parse_request_body(
    arg: &FnArg,
    known_schemas: &std::collections::HashMap<String, String>,
    struct_definitions: &std::collections::HashMap<String, String>,
) -> Option<RequestBody> {
    match arg {
        FnArg::Receiver(_) => None,
        FnArg::Typed(PatType { ty, .. }) => {
            if let Type::Path(type_path) = ty.as_ref() {
                let path = &type_path.path;
                if path.segments.is_empty() {
                    return None;
                }

                // Check the last segment (handles both Json<T> and vespera::axum::Json<T>)
                let segment = path.segments.last().unwrap();
                let ident_str = segment.ident.to_string();

                if ident_str == "Json"
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                {
                    let schema = parse_type_to_schema_ref_with_schemas(
                        inner_ty,
                        known_schemas,
                        struct_definitions,
                    );
                    let mut content = BTreeMap::new();
                    content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(schema),
                            example: None,
                            examples: None,
                        },
                    );
                    return Some(RequestBody {
                        description: None,
                        required: Some(true),
                        content,
                    });
                }
            }

            if is_string_like(ty.as_ref()) {
                let schema =
                    parse_type_to_schema_ref_with_schemas(ty, known_schemas, struct_definitions);
                let mut content = BTreeMap::new();
                content.insert(
                    "text/plain".to_string(),
                    MediaType {
                        schema: Some(schema),
                        example: None,
                        examples: None,
                    },
                );

                return Some(RequestBody {
                    description: None,
                    required: Some(true),
                    content,
                });
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;
    use std::collections::HashMap;
    use vespera_core::schema::{SchemaRef, SchemaType};

    #[rstest]
    #[case::json("fn test(Json(payload): Json<User>) {}", true, "json")]
    #[case::string("fn test(just_string: String) {}", true, "string")]
    #[case::str("fn test(just_str: &str) {}", true, "str")]
    #[case::i32("fn test(just_i32: i32) {}", false, "i32")]
    fn test_parse_request_body_cases(
        #[case] func_src: &str,
        #[case] has_body: bool,
        #[case] suffix: &str,
    ) {
        let func: syn::ItemFn = syn::parse_str(func_src).unwrap();
        let arg = func.sig.inputs.first().unwrap();
        let body = parse_request_body(arg, &HashMap::new(), &HashMap::new());
        assert_eq!(body.is_some(), has_body);
        with_settings!({ snapshot_suffix => format!("req_body_{}", suffix) }, {
            assert_debug_snapshot!(body);
        });
    }

    #[test]
    fn test_parse_request_body_text_plain_schema() {
        let func: syn::ItemFn = syn::parse_str("fn test(body: &str) {}").unwrap();
        let arg = func.sig.inputs.first().unwrap();
        let body = parse_request_body(arg, &HashMap::new(), &HashMap::new())
            .expect("expected request body");

        let media = body
            .content
            .get("text/plain")
            .expect("expected text/plain content");

        if let SchemaRef::Inline(schema) = media.schema.as_ref().expect("schema expected") {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
        } else {
            panic!("expected inline schema");
        }
    }
}
