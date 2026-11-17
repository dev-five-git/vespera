//! Round-18 fixes: garde rules naming a sibling field the DTO renames,
//! drops or wraps alone, and the `Vec` parts `add` gives a multipart DTO.

use std::collections::HashMap;

use rstest::rstest;

use super::*;

const SIGNUP: &str = r"pub struct Signup {
    #[garde(length(min = 8))]
    pub password: String,
    #[garde(matches(password), custom(differs(&self.password)))]
    pub confirm: String,
}";

fn generate(input: &str) -> syn::Result<String> {
    let storage = HashMap::from([(
        "Signup".to_string(),
        StructMetadata::new("Signup".to_string(), SIGNUP.to_string()),
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let (tokens, _) = generate_schema_type_code(&input, &storage)?;
    Ok(tokens.to_string())
}

/// L1: a `rename`d peer is renamed in the rules too (E0609 before).
#[test]
fn renamed_garde_peer_is_followed() {
    let code = generate(r#"Dto from Signup, rename = [("password", "pw")]"#).unwrap();
    assert!(
        code.contains("# [garde (matches (pw) , custom (differs (& self . pw)))] pub confirm"),
        "{code}"
    );
}

/// L1: only the rule's field wrapped (the rule moves into `inner(..)`, and
/// `matches` compares the inner values) still compiles; no diagnostic.
#[rstest]
#[case::field_partial(r#"Dto from Signup, partial = ["confirm"]"#)]
fn wrapped_garde_rule_field_is_accepted(#[case] input: &str) {
    assert!(generate(input).is_ok());
}

/// L1: a peer the DTO drops, or wraps in `Option` alone, is a
/// `schema_type!` diagnostic (E0609 / E0277 in generated code before).
#[rstest]
#[case::omitted(r#"Dto from Signup, omit = ["password"]"#, "`omit` drops it")]
#[case::not_picked(r#"Dto from Signup, pick = ["confirm"]"#, "`pick` does not keep it")]
#[case::partial_peer(
    r#"Dto from Signup, partial = ["password"]"#,
    "`#[garde(matches(password))]` on field `confirm` compares it with `password`, which `partial` wraps in `Option`"
)]
// `self.password` reads the `Option<String>` the DTO field becomes.
#[case::partial_self_read(
    "Dto from Signup, partial",
    "reads `self.password`, which `partial` wraps in `Option`"
)]
fn broken_garde_peer_is_reported(#[case] input: &str, #[case] message: &str) {
    let err = generate(input).unwrap_err().to_string();
    assert!(err.contains(message), "{err}");
}

/// L2: a bare `Vec` field `add` gives a multipart DTO reads "no parts" as
/// `[]` like a source one, so it gets `#[form_data(default)]` (the request
/// schema listed it as required); an `Option` / scalar one does not.
#[test]
fn multipart_add_vec_field_defaults() {
    let storage = HashMap::from([(
        "Upload".to_string(),
        StructMetadata::new(
            "Upload".to_string(),
            "pub struct Upload { pub title: String }".to_string(),
        ),
    )]);
    let input: SchemaTypeInput = syn::parse_str(
        r#"UploadForm from Upload, multipart, name = "UploadFormSchema", add = [("tags": Vec<String>), ("note": Option<Vec<String>>), ("count": u8)]"#,
    )
    .unwrap();
    let (tokens, metadata) = generate_schema_type_code(&input, &storage).unwrap();
    let code = tokens.to_string();
    assert_eq!(code.matches("# [form_data (default)]").count(), 1, "{code}");
    assert!(
        code.contains("# [form_data (default)] pub tags : Vec < String >"),
        "{code}"
    );
    let metadata = metadata.unwrap().definition;
    assert!(
        metadata.contains("# [form_data (default)] pub tags : Vec < String >"),
        "{metadata}"
    );
}

/// L7: `omit_default` drops a SeaORM 2 `default_expr` column too (it stayed
/// a required field of the create DTO).
#[test]
fn omit_default_drops_a_default_expr_column() {
    let storage = HashMap::from([(
        "Model".to_string(),
        StructMetadata::new(
            "Model".to_string(),
            r#"pub struct Model { #[sea_orm(primary_key)] pub id: i32, pub name: String, #[sea_orm(default_expr = "Expr::current_timestamp()")] pub created_at: String }"#.to_string(),
        ),
    )]);
    let input: SchemaTypeInput = syn::parse_str("CreateDto from Model, omit_default").unwrap();
    let code = generate_schema_type_code(&input, &storage)
        .unwrap()
        .0
        .to_string();
    assert!(code.contains("pub name : String"), "{code}");
    assert!(!code.contains("created_at"), "{code}");
}
