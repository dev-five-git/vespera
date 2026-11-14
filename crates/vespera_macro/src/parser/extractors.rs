use syn::{GenericArgument, PathArguments, Type, punctuated::Punctuated, token::Comma};

/// The extractor a handler argument documents: `ty` with every
/// `Validated<E>` / `ValidatedWith<C, E>`, `Result<E, Rejection>`,
/// `axum_extra::extract::WithRejection<E, R>` and
/// `axum_extra::extract::Cached<E>` wrapper removed (in any nesting). A
/// `Result` / `WithRejection` argument only changes how the rejection is
/// answered and `Cached` only memoizes the extraction: the request still
/// carries — and is documented with — what `E` extracts. `Option<E>` is kept:
/// it changes requiredness, so each caller unwraps it itself.
pub fn request_extractor(mut ty: &Type) -> &Type {
    while let Some(inner) = validated_inner_type(ty).or_else(|| transparent_inner(ty)) {
        ty = inner;
    }
    ty
}

/// `E` of a wrapper that extracts exactly what `E` extracts: `Result<E, _>`,
/// `WithRejection<E, R>` and `Cached<E>` (their FIRST type argument).
fn transparent_inner(ty: &Type) -> Option<&Type> {
    result_ok_type(ty)
        .or_else(|| extractor_inner_type(ty, "WithRejection"))
        .or_else(|| extractor_inner_type(ty, "Cached"))
}

/// `ty` with every [`transparent_inner`] wrapper removed (`Validated` kept).
#[cfg(feature = "validation")]
fn strip_transparent(mut ty: &Type) -> &Type {
    while let Some(inner) = transparent_inner(ty) {
        ty = inner;
    }
    ty
}

/// `T` of `Result<T, E>` / `std::result::Result<T, E>` /
/// `core::result::Result<T, E>` (two type arguments: a one-argument
/// `io::Result<T>`-style alias is not the prelude `Result`).
fn result_ok_type(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    let is_result = match segments.as_slice() {
        [result] => result == "Result" && type_path.path.leading_colon.is_none(),
        [krate, module, result] => {
            (krate == "std" || krate == "core") && module == "result" && result == "Result"
        }
        _ => false,
    };
    let args = extractor_args(ty, "Result").filter(|args| is_result && args.len() == 2)?;
    as_type(args.first())
}

/// Return true when the type is a `Validated<...>` or `ValidatedWith<..., ...>`
/// extractor wrapper.
pub(super) fn is_validated_type(ty: &Type) -> bool {
    validated_inner_type(ty).is_some()
}

/// The extractor wrapped by `Validated<T>` (its first generic argument) or by
/// `ValidatedWith<C, T>` (its LAST generic argument — the first one is the
/// validation context, which never reaches the wire).
pub(super) fn validated_inner_type(ty: &Type) -> Option<&Type> {
    extractor_inner_type(ty, "Validated")
        .or_else(|| as_type(extractor_args(ty, "ValidatedWith")?.last()))
}

/// Payload-carrying extractors that `vespera::Validated` / `ValidatedWith`
/// support out of the box (`ValidatePayloadWith` impls in `vespera`).
#[cfg(feature = "validation")]
const PAYLOAD_EXTRACTORS: [&str; 5] = ["Json", "Form", "Query", "Path", "TypedMultipart"];

/// The validated payload `U` of a `Validated<E<U>>` / `ValidatedWith<C, E<U>>`
/// argument, where `E` is one of the built-in payload extractors. A custom
/// extractor (the user's own `ValidatePayload` impl) yields `None` — its
/// payload type cannot be named syntactically.
#[cfg(feature = "validation")]
pub fn validated_payload_type(ty: &Type) -> Option<&Type> {
    // `Result<Validated<E<U>>, _>` / `WithRejection<Validated<E<U>>, R>` still
    // validate `U` (only the rejection is answered differently).
    let extractor = validated_inner_type(strip_transparent(ty))?;
    PAYLOAD_EXTRACTORS
        .iter()
        .find_map(|name| extractor_inner_type(extractor, name))
}

/// Extract the first generic type argument from an extractor by final path segment.
pub(super) fn extractor_inner_type<'a>(ty: &'a Type, extractor: &str) -> Option<&'a Type> {
    as_type(extractor_args(ty, extractor)?.first())
}

/// The angle-bracketed generic arguments of `ty` when its final path segment
/// is `extractor`.
fn extractor_args<'a>(
    ty: &'a Type,
    extractor: &str,
) -> Option<&'a Punctuated<GenericArgument, Comma>> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != extractor {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    Some(&args.args)
}

/// `Some(ty)` when `arg` is a type argument (not a lifetime / const).
const fn as_type(arg: Option<&GenericArgument>) -> Option<&Type> {
    match arg {
        Some(GenericArgument::Type(inner_ty)) => Some(inner_ty),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::validated("vespera::Validated<axum::Json<User>>", "axum :: Json < User >")]
    #[case::validated_with("ValidatedWith<Ctx, Json<User>>", "Json < User >")]
    #[case::validated_with_qualified(
        "vespera::ValidatedWith<crate::Ctx, axum::extract::Query<Filter>>",
        "axum :: extract :: Query < Filter >"
    )]
    fn unwraps_validated_inner_extractor(#[case] src: &str, #[case] expected: &str) {
        let ty: Type = syn::parse_str(src).unwrap();
        assert!(is_validated_type(&ty));
        let inner = request_extractor(&ty);
        assert_eq!(quote::quote!(#inner).to_string(), expected);
    }

    #[rstest]
    #[case::result("Result<Json<User>, JsonRejection>", "Json < User >")]
    #[case::std_result("std::result::Result<Query<F>, R>", "Query < F >")]
    #[case::core_result("::core::result::Result<Path<u32>, R>", "Path < u32 >")]
    #[case::result_validated("Result<Validated<Json<U>>, R>", "Json < U >")]
    #[case::validated_result("Validated<Result<Json<U>, R>>", "Json < U >")]
    #[case::option_kept("Option<Json<U>>", "Option < Json < U > >")]
    #[case::one_argument_alias("io::Result<Json<U>>", "io :: Result < Json < U > >")]
    #[case::bare_one_argument("Result<Json<U>>", "Result < Json < U > >")]
    #[case::leading_colon_bare("::Result<Json<U>, R>", ":: Result < Json < U > , R >")]
    #[case::other_crate(
        "my::result::Result<Json<U>, R>",
        "my :: result :: Result < Json < U > , R >"
    )]
    #[case::qself(
        "<T as Tr>::Result<Json<U>, R>",
        "< T as Tr > :: Result < Json < U > , R >"
    )]
    #[case::lifetime_first("Result<'a, R>", "Result < 'a , R >")]
    #[case::reference("&Json<U>", "& Json < U >")]
    #[case::with_rejection("WithRejection<Json<U>, ApiError>", "Json < U >")]
    #[case::with_rejection_qualified(
        "axum_extra::extract::WithRejection<Query<F>, ApiError>",
        "Query < F >"
    )]
    #[case::cached("axum_extra::extract::Cached<Path<u32>>", "Path < u32 >")]
    #[case::cached_with_rejection_validated(
        "Cached<WithRejection<Validated<Form<U>>, R>>",
        "Form < U >"
    )]
    #[case::bare_with_rejection("WithRejection", "WithRejection")]
    #[case::cached_lifetime("Cached<'a>", "Cached < 'a >")]
    fn request_extractor_unwraps_result_and_validated(#[case] src: &str, #[case] expected: &str) {
        let ty: Type = syn::parse_str(src).unwrap();
        let inner = request_extractor(&ty);
        assert_eq!(quote::quote!(#inner).to_string(), expected);
    }

    #[cfg(feature = "validation")]
    #[rstest]
    #[case::result_validated_json("Result<Validated<Json<User>>, R>", Some("User"))]
    #[case::json("Validated<Json<User>>", Some("User"))]
    #[case::form("Validated<axum::Form<User>>", Some("User"))]
    #[case::query("ValidatedWith<Ctx, Query<Filter>>", Some("Filter"))]
    #[case::path("Validated<Path<(i64, String)>>", Some("(i64 , String)"))]
    #[case::multipart("Validated<TypedMultipart<Upload>>", Some("Upload"))]
    #[case::custom_extractor("Validated<MyExtractor<User>>", None)]
    #[case::with_rejection_validated("WithRejection<Validated<Json<User>>, R>", Some("User"))]
    #[case::cached_result_validated("Cached<Result<Validated<Query<F>>, R>>", Some("F"))]
    #[case::not_validated("Json<User>", None)]
    fn finds_validated_payload(#[case] src: &str, #[case] expected: Option<&str>) {
        let ty: Type = syn::parse_str(src).unwrap();
        let payload = validated_payload_type(&ty).map(|p| quote::quote!(#p).to_string());
        assert_eq!(payload.as_deref(), expected);
    }

    #[rstest]
    #[case::reference("&str")]
    #[case::bare("Validated")]
    #[case::lifetime("Validated<'static>")]
    #[case::bare_with("ValidatedWith")]
    #[case::with_lifetime_last("ValidatedWith<Ctx, 'static>")]
    #[case::with_empty("ValidatedWith<>")]
    #[case::plain_json("Json<User>")]
    fn rejects_non_validated_shapes(#[case] src: &str) {
        let ty: Type = syn::parse_str(src).unwrap();
        assert!(validated_inner_type(&ty).is_none());
        assert!(!is_validated_type(&ty));
        assert!(std::ptr::eq(request_extractor(&ty), &raw const ty));
    }
}
