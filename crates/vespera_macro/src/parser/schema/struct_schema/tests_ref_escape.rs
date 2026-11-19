//! A `#[schema(ref = "..")]` override is written into the `$ref` escaped
//! (JSON Pointer + percent-encoding), so a name with `/`, `~` or a space
//! resolves to its own component instead of a path below it.

use std::collections::{HashMap, HashSet};

use super::*;

fn ref_of(src: &str) -> Option<String> {
    let item: syn::ItemStruct = syn::parse_str(src).unwrap();
    let (known, definitions) = (HashSet::<String>::new(), HashMap::<String, String>::new());
    parse_struct_to_schema(&item, &known, &definitions).ref_path
}

#[test]
fn ref_name_with_a_slash_is_escaped_in_its_ref() {
    assert_eq!(
        ref_of(r#"#[schema(ref = "Pet/Dog")] struct Dog { id: u32 }"#).as_deref(),
        Some("#/components/schemas/Pet~1Dog"),
    );
}

#[test]
fn ref_name_with_a_tilde_and_space_is_escaped_in_its_ref() {
    assert_eq!(
        ref_of(r#"#[schema(ref = "My~Dto X")] struct Dto { id: u32 }"#).as_deref(),
        Some("#/components/schemas/My~0Dto%20X"),
    );
}

#[test]
fn plain_ref_name_keeps_its_ref() {
    assert_eq!(
        ref_of(r#"#[schema(ref = "UserSchema")] struct User { id: u32 }"#).as_deref(),
        Some("#/components/schemas/UserSchema"),
    );
}
