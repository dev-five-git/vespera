use std::collections::{HashMap, HashSet};

use rstest::rstest;
use vespera_core::schema::Schema;

use super::*;
use crate::parser::schema::parse_struct_to_schema;

/// The `value` property schema of `struct S { <field_src> }`, as pretty JSON.
fn field_json(field_src: &str) -> String {
    let struct_item: syn::ItemStruct =
        syn::parse_str(&format!("struct S {{ {field_src} }}")).unwrap();
    let known: HashSet<String> = std::iter::once("User".to_string()).collect();
    let schema: Schema =
        parse_struct_to_schema(&struct_item, &known, &HashMap::<String, String>::new());
    let property = &schema.properties.as_ref().unwrap()["value"];
    serde_json::to_string_pretty(property).unwrap()
}

#[rstest]
#[case::length_chars(
    "length_chars",
    "#[garde(length(chars, min = 2, max = 20))] value: String"
)]
#[case::length_chars_equal(
    "length_chars_equal",
    "#[garde(length(chars, equal = 2))] value: String"
)]
#[case::length_simple_min_one(
    "length_simple_min_one",
    "#[garde(length(min = 1, max = 50))] value: String"
)]
#[case::length_explicit_simple(
    "length_explicit_simple",
    "#[garde(length(simple, min = 1))] value: String"
)]
#[case::length_bytes_skipped(
    "length_bytes_skipped",
    "#[garde(length(bytes, min = 3, max = 9))] value: String"
)]
#[case::length_const_skipped(
    "length_const_skipped",
    "#[garde(length(chars, min = MIN))] value: String"
)]
#[case::length_vec_items(
    "length_vec_items",
    "#[garde(length(min = 1, max = 3))] value: Vec<String>"
)]
#[case::length_vec_non_simple(
    "length_vec_non_simple",
    "#[garde(length(chars, min = 1))] value: Vec<String>"
)]
#[case::length_map_properties(
    "length_map_properties",
    "#[garde(length(min = 1, max = 4))] value: HashMap<String, i32>"
)]
#[case::range_int("range_int", "#[garde(range(min = 18, max = 120))] value: i32")]
#[case::range_equal("range_equal", "#[garde(range(equal = 5))] value: i64")]
#[case::range_negative_float("range_negative_float", "#[garde(range(min = -1.5))] value: f64")]
#[case::range_const_skipped(
    "range_const_skipped",
    "#[garde(range(min = LOW, max = 9))] value: i32"
)]
#[case::range_tightens_type_bound(
    "range_tightens_type_bound",
    "#[garde(range(min = 5))] value: u32"
)]
#[case::range_keeps_tighter_type_bound(
    "range_keeps_tighter_type_bound",
    "#[garde(range(min = -5))] value: u128"
)]
#[case::email("email", "#[garde(email)] value: String")]
#[case::url("url", "#[garde(url)] value: String")]
#[case::ipv4("ipv4", "#[garde(ipv4)] value: String")]
#[case::ipv6("ipv6", "#[garde(ipv6)] value: String")]
#[case::ip("ip", "#[garde(ip)] value: String")]
#[case::pattern("pattern", r#"#[garde(pattern(r"^[A-Z]{2}$"))] value: String"#)]
#[case::pattern_static_skipped("pattern_static_skipped", "#[garde(pattern(RE))] value: String")]
#[case::prefix_suffix_contains(
    "prefix_suffix_contains",
    r#"#[garde(prefix("a.b"), suffix("(x)"), contains("$^|?*+[]{}\\"))] value: String"#
)]
#[case::ascii("ascii", "#[garde(ascii)] value: String")]
#[case::optional_string(
    "optional_string",
    "#[garde(length(chars, min = 1), email)] value: Option<String>"
)]
#[case::inner_vec_items(
    "inner_vec_items",
    r##"#[garde(length(min = 1), inner(length(chars, max = 8), prefix("#")))] value: Vec<String>"##
)]
#[case::inner_array_items("inner_array_items", "#[garde(inner(ascii))] value: [String; 2]")]
#[case::inner_option_value(
    "inner_option_value",
    "#[garde(inner(length(chars, min = 1)))] value: Option<String>"
)]
#[case::inner_option_vec(
    "inner_option_vec",
    "#[garde(inner(length(min = 1), inner(range(max = 9))))] value: Option<Vec<u8>>"
)]
#[case::inner_ref_items_skipped(
    "inner_ref_items_skipped",
    "#[garde(inner(length(min = 1)))] value: Vec<User>"
)]
#[case::inner_no_target(
    "inner_no_target",
    "#[garde(inner(length(chars, min = 1)))] value: String"
)]
#[case::ref_field_skipped("ref_field_skipped", "#[garde(length(min = 1))] value: User")]
#[case::bool_field_skipped("bool_field_skipped", "#[garde(email)] value: bool")]
#[case::skipped_rules(
    "skipped_rules",
    "#[garde(alphanumeric, required, credit_card, phone_number, skip, dive, matches(other), \
     custom(|v: &String, _: &()| Ok(())), code = \"x\", length(chars, max = 4))] value: String"
)]
#[case::malformed_attr_ignored(
    "malformed_attr_ignored",
    "#[garde(email)] #[garde(ascii, length(min))] value: String"
)]
#[case::explicit_wins(
    "explicit_wins",
    r#"#[schema(min_length = 5, pattern = "^x", format = "hostname")]
       #[garde(length(chars, min = 2, max = 10), email, prefix("y"), suffix("z"))]
       value: String"#
)]
#[case::explicit_numeric_wins(
    "explicit_numeric_wins",
    "#[schema(maximum = 3)] #[garde(range(min = 1, max = 9))] value: i32"
)]
#[case::explicit_items_wins(
    "explicit_items_wins",
    "#[schema(min_items = 2)] #[garde(length(min = 1, max = 5))] value: Vec<i32>"
)]
#[case::any_skips_garde(
    "any_skips_garde",
    "#[schema(any)] #[garde(length(min = 1))] value: HashMap<String, i32>"
)]
fn garde_rules_reflect_into_field_schema(#[case] name: &str, #[case] field_src: &str) {
    insta::assert_snapshot!(format!("garde_rules_{name}"), field_json(field_src));
}

#[rstest]
#[case::lower_raises(Some(1), None, Some(5), true, Some(5))]
#[case::lower_keeps_tighter(Some(9), None, Some(5), true, Some(9))]
#[case::upper_lowers(Some(9), None, Some(5), false, Some(5))]
#[case::upper_keeps_tighter(Some(3), None, Some(5), false, Some(3))]
#[case::equal_bound(Some(5), None, Some(5), true, Some(5))]
#[case::unset(None, None, Some(5), false, Some(5))]
#[case::explicit_blocks(Some(1), Some(2), Some(5), true, Some(1))]
#[case::nothing_derived(Some(1), None, None, true, Some(1))]
fn tighten_cases(
    #[case] current: Option<usize>,
    #[case] explicit: Option<usize>,
    #[case] derived: Option<usize>,
    #[case] lower_bound: bool,
    #[case] expected: Option<usize>,
) {
    let mut slot = current;
    tighten(&mut slot, explicit, derived, lower_bound);
    assert_eq!(slot, expected);
}

#[test]
fn unrelated_attributes_yield_no_rules() {
    let item: syn::ItemStruct = syn::parse_quote!(
        struct S {
            #[serde(rename = "x")]
            #[schema(min_length = 1)]
            value: String,
        }
    );
    let field = item.fields.iter().next().unwrap();
    assert_eq!(extract_garde_rules(&field.attrs), GardeRules::default());
}

#[test]
fn escape_regex_escapes_every_syntax_character() {
    assert_eq!(
        escape_regex(r"a\^$.|?*+()[]{}-/#"),
        r"a\\\^\$\.\|\?\*\+\(\)\[\]\{\}-/#"
    );
}
