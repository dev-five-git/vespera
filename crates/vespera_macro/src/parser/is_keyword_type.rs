use syn::{Type, TypePath};

#[allow(dead_code)]
pub enum KeywordType {
    HeaderMap,
    StatusCode,
    Json,
    Path,
    Query,
    Header,
    TypedHeader,
    Result,
}

impl KeywordType {
    pub fn as_str(&self) -> &str {
        match self {
            KeywordType::HeaderMap => "HeaderMap",
            KeywordType::StatusCode => "StatusCode",
            KeywordType::Json => "Json",
            KeywordType::Path => "Path",
            KeywordType::Query => "Query",
            KeywordType::Header => "Header",
            KeywordType::TypedHeader => "TypedHeader",
            KeywordType::Result => "Result",
        }
    }
}

pub fn is_keyword_type(ty: &Type, keyword: &KeywordType) -> bool {
    if let Type::Path(type_path) = ty {
        is_keyword_type_by_type_path(type_path, keyword)
    } else {
        false
    }
}

pub fn is_keyword_type_by_type_path(ty: &TypePath, keyword: &KeywordType) -> bool {
    ty.path.segments.last().unwrap().ident == keyword.as_str()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use syn::parse_str;

    use super::*;

    fn syn_type(ty: &str) -> Type {
        parse_str::<Type>(ty).expect("Failed to parse type")
    }

    #[rstest]
    #[case("HeaderMap", KeywordType::HeaderMap, true)]
    #[case("StatusCode", KeywordType::StatusCode, true)]
    #[case("Json", KeywordType::Json, true)]
    #[case("Path", KeywordType::Path, true)]
    #[case("Query", KeywordType::Query, true)]
    #[case("Header", KeywordType::Header, true)]
    #[case("TypedHeader", KeywordType::TypedHeader, true)]
    #[case("String", KeywordType::HeaderMap, false)]
    #[case("HeaderMap", KeywordType::Json, false)]
    #[case("axum::http::HeaderMap", KeywordType::HeaderMap, true)]
    #[case("axum::http::StatusCode", KeywordType::StatusCode, true)]
    #[case("othermod::Json", KeywordType::Json, true)]
    #[case("CustomType", KeywordType::Path, false)]
    #[case("Result", KeywordType::Result, true)]
    #[case("Result<String, String>", KeywordType::Result, true)]
    #[case("!", KeywordType::Result, false)]
    fn test_is_keyword_type(
        #[case] ty_str: &str,
        #[case] keyword: KeywordType,
        #[case] expected: bool,
    ) {
        let ty = syn_type(ty_str);
        assert_eq!(is_keyword_type(&ty, &keyword), expected);
    }
}
