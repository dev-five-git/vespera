//! The name a `#[cfg]` twin's generated helpers are keyed by (R44-SM2-L1).

use std::collections::HashSet;

use crate::parser::strip_raw_prefix_owned;

/// The key the helpers of field `index` (named `name`, one of several `#[cfg]`
/// twins) are generated under: `{name}_{index}`, bumped with `_` while a field
/// of the source struct already has that name. Without the bump a real field
/// `owner_1` collided with the twin `owner` at index 1 (`default_Dto_owner_1`
/// and `__vespera_Dto_owner_1_ser` declared twice, E0428).
pub fn twin_helper_key(fields: &syn::Fields, name: &str, index: usize) -> String {
    let taken: HashSet<String> = fields
        .iter()
        .filter_map(|field| field.ident.as_ref())
        .map(|ident| strip_raw_prefix_owned(ident.to_string()))
        .collect();
    let mut key = format!("{name}_{index}");
    while taken.contains(&key) {
        key.push('_');
    }
    key
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::unused_name("struct S { owner: i32, owner_2: i32 }", "owner_1")]
    #[case::real_field_takes_the_key("struct S { owner: i32, owner_1: i32 }", "owner_1_")]
    #[case::bumped_key_is_taken_too("struct S { owner_1: i32, owner_1_: i32 }", "owner_1__")]
    #[case::raw_identifier_counts("struct S { r#owner_1: i32 }", "owner_1_")]
    fn twin_key_matches_no_source_field(#[case] source: &str, #[case] expected: &str) {
        let item: syn::ItemStruct = syn::parse_str(source).unwrap();
        assert_eq!(twin_helper_key(&item.fields, "owner", 1), expected);
    }
}
