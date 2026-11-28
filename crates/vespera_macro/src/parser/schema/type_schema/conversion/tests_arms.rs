//! Pins `is_builtin_openapi_type` to the leaf conversion arms of
//! `parse_type_impl`, in both directions (sidecar of `conversion`; the
//! `include_str!` path is this directory's `../conversion.rs`).

use super::is_builtin_openapi_type;

/// Every ident [`is_builtin_openapi_type`] lists has its own arm in
/// `parse_type_impl`'s leaf `primitive_result` match, or — for the few
/// generic-only builtins — in its generic match (a listed ident without one
/// skips the `T: Schema` assertion yet maps to the unknown-type `{}`).
#[test]
fn is_builtin_openapi_type_matches_conversion_arms() {
    use proc_macro2::{Delimiter, TokenStream, TokenTree};

    fn literals(tokens: TokenStream, out: &mut Vec<String>) {
        for tree in tokens {
            match tree {
                TokenTree::Group(group) => literals(group.stream(), out),
                TokenTree::Literal(lit) => out.push(lit.to_string()),
                _ => {}
            }
        }
    }
    /// The brace group (match arms) of the first `match` after the ident
    /// `binding`, searched through every nested group.
    fn match_arms_after(tokens: TokenStream, binding: &str) -> Option<TokenStream> {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        if let Some(start) = trees
            .iter()
            .position(|tree| matches!(tree, TokenTree::Ident(ident) if ident == binding))
        {
            let after_match = trees[start..]
                .iter()
                .position(|tree| matches!(tree, TokenTree::Ident(ident) if ident == "match"))?;
            return trees[start + after_match..]
                .iter()
                .find_map(|tree| match tree {
                    TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                        Some(group.stream())
                    }
                    _ => None,
                });
        }
        trees.into_iter().find_map(|tree| match tree {
            TokenTree::Group(group) => match_arms_after(group.stream(), binding),
            _ => None,
        })
    }
    let file = syn::parse_file(include_str!("../conversion.rs")).unwrap();
    let block = |name: &str| {
        file.items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(func) if func.sig.ident == name => {
                    Some(quote::ToTokens::to_token_stream(&func.block))
                }
                _ => None,
            })
            .unwrap()
    };
    let mut builtins = Vec::new();
    literals(block("is_builtin_openapi_type"), &mut builtins);
    let builtins: Vec<String> = builtins
        .into_iter()
        .filter_map(|lit| lit.strip_prefix('b').map(str::to_string))
        .collect();
    // Patterns only: a string in an arm body (`"int8"`) is not a leaf arm.
    let mut leaf_arms = Vec::new();
    arm_patterns(
        match_arms_after(block("parse_type_impl"), "primitive_result").unwrap(),
        &mut leaf_arms,
    );
    let mut all_arms = Vec::new();
    literals(block("parse_type_impl"), &mut all_arms);
    // Builtins only meaningful with type arguments: their arm is in the
    // generic (`AngleBracketed`) match, not the leaf one.
    let generic_only = ["HashMap", "BTreeMap", "HasOne", "HasMany", "BelongsTo"];
    assert!(builtins.len() > 50, "{builtins:?}");
    assert!(leaf_arms.len() > 50, "{leaf_arms:?}");
    for builtin in &builtins {
        let ident = builtin.trim_matches('"');
        if generic_only.contains(&ident) {
            assert!(!leaf_arms.contains(builtin), "{builtin} has a leaf arm");
            assert!(all_arms.contains(builtin), "no generic arm for {builtin}");
        } else {
            assert!(
                leaf_arms.contains(builtin),
                "no leaf conversion arm for {builtin}"
            );
        }
        assert!(is_builtin_openapi_type(ident));
    }
    // The other direction: every pattern of a leaf arm is a builtin, so a leaf
    // arm cannot be added without its `is_builtin_openapi_type` entry.
    let mut patterns = Vec::new();
    arm_patterns(
        match_arms_after(block("parse_type_impl"), "primitive_result").unwrap(),
        &mut patterns,
    );
    assert!(patterns.len() > 50, "{patterns:?}");
    for pattern in &patterns {
        assert!(
            builtins.contains(pattern),
            "leaf arm {pattern} is not in is_builtin_openapi_type"
        );
    }
}

/// The literal patterns of the match arms in `tokens`: the literals before
/// each top-level `=>`. A guard, an arm body (format strings such as
/// `"int8"`) and a block body's end (no trailing comma) are not patterns.
fn arm_patterns(tokens: proc_macro2::TokenStream, out: &mut Vec<String>) {
    use proc_macro2::{Delimiter, TokenTree};

    let mut in_pattern = true;
    for tree in tokens {
        match tree {
            TokenTree::Punct(punct) if in_pattern && punct.as_char() == '=' => {
                in_pattern = false;
            }
            TokenTree::Punct(punct) if !in_pattern && punct.as_char() == ',' => {
                in_pattern = true;
            }
            TokenTree::Group(group) if !in_pattern && group.delimiter() == Delimiter::Brace => {
                in_pattern = true;
            }
            TokenTree::Literal(lit) if in_pattern => out.push(lit.to_string()),
            _ => {}
        }
    }
}

/// `BinaryHeap<T>` is a std sequence like `VecDeque` / `LinkedList`: a builtin
/// that documents an array of its element (M44-MR4-1, round 44). Without the
/// arm it fell through to an unknown `$ref`, and the leaf assertion did not
/// exempt it either.
#[test]
fn binary_heap_is_a_builtin_array_of_its_element() {
    use std::collections::{HashMap, HashSet};

    use vespera_core::schema::{SchemaRef, SchemaType};

    use super::parse_type_to_schema_ref;

    assert!(is_builtin_openapi_type("BinaryHeap"));
    let ty: syn::Type = syn::parse_str("BinaryHeap<u32>").unwrap();
    let known: HashSet<String> = HashSet::new();
    let defs: HashMap<String, String> = HashMap::new();
    let SchemaRef::Inline(schema) = parse_type_to_schema_ref(&ty, &known, &defs) else {
        panic!("BinaryHeap<u32> is an inline array");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::Array));
    let Some(SchemaRef::Inline(items)) = schema.items else {
        panic!("items is the inline u32 schema");
    };
    assert_eq!(items.schema_type, Some(SchemaType::Integer));
}

/// Round 44: a bare `Map<K, V>` (`use serde_json::Map;`) is the JSON object the
/// qualified spelling is, unless it names a known component schema (L44-MR4-2).
#[test]
fn bare_map_is_a_json_object_unless_it_is_a_known_schema() {
    use std::collections::{HashMap, HashSet};

    use vespera_core::schema::{SchemaRef, SchemaType};

    use super::parse_type_to_schema_ref;

    let ty: syn::Type = syn::parse_str("Map<String, u8>").unwrap();
    let defs: HashMap<String, String> = HashMap::new();
    let SchemaRef::Inline(schema) = parse_type_to_schema_ref(&ty, &HashSet::<String>::new(), &defs)
    else {
        panic!("a bare Map is an inline object");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::Object));
    let known: HashSet<String> = HashSet::from(["Map".to_string()]);
    assert!(matches!(
        parse_type_to_schema_ref(&ty, &known, &defs),
        SchemaRef::Ref(_)
    ));
}
