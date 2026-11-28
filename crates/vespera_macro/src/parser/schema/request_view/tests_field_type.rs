//! Whether a field's documented type differs per side (`field_type_differs`):
//! a `value_type` decides it alone, `#[schema(any)]` documents `{}` on both
//! sides (sidecar of `request_view`; `use super::*` semantics).

use rstest::rstest;

use super::*;

/// Round 44: the Rust type of a field a `value_type` or `#[schema(any)]`
/// replaces is not what the view documents, so it must not make the view
/// differ (`HashSet` is the one std type whose schema differs per side).
#[rstest]
#[case::plain_set("struct A { tags: HashSet<String> }", true)]
#[case::value_type_replaces_set(
    "struct A { #[schema(value_type = Vec<u8>)] tags: HashSet<String> }",
    false
)]
#[case::any_replaces_set("struct A { #[schema(any)] tags: HashSet<String> }", false)]
#[case::value_type_set_over_vec(
    "struct A { #[schema(value_type = HashSet<String>)] tags: Vec<u8> }",
    true
)]
fn field_documented_type_decides_the_view(#[case] src: &str, #[case] expected: bool) {
    let item: syn::Item = syn::parse_str(src).unwrap();
    let flat = FlatKinds::of(&[]);
    assert_eq!(directly_differs(&item, &flat), expected, "{src}");
}
