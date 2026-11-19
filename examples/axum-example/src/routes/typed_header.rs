use vespera::axum_extra::{
    TypedHeader,
    headers::{Authorization, ContentType, UserAgent, authorization::Bearer},
};

#[vespera::route(post)]
pub async fn typed_header(
    TypedHeader(user_agent): TypedHeader<UserAgent>,
    content_type: Option<TypedHeader<ContentType>>,
) -> &'static str {
    println!("user_agent: {:?}", user_agent);
    println!("content_type: {:?}", content_type);
    "ok"
}

#[vespera::route()]
pub async fn typed_header_jwt(
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
) -> &'static str {
    // Never print the credential: `Debug` of `Authorization<Bearer>` is the
    // whole token, and logs keep what they capture.
    println!(
        "authorization: bearer token of {} bytes",
        authorization.token().len()
    );
    "ok"
}
