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
// Round 11: garde's `Inner` impls for `Box<[T]>` / `&[T]`.
#[case::inner_boxed_slice_items(
    "inner_boxed_slice_items",
    "#[garde(inner(length(chars, max = 8)))] value: Box<[String]>"
)]
#[case::inner_slice_ref_items(
    "inner_slice_ref_items",
    "#[garde(inner(length(chars, max = 8)))] value: &'a [String]"
)]
#[case::inner_box_non_slice_no_target(
    "inner_box_non_slice_no_target",
    "#[garde(inner(length(chars, max = 8)))] value: Box<String>"
)]
#[case::inner_map_no_target(
    "inner_map_no_target",
    "#[garde(inner(length(chars, max = 8)))] value: HashMap<String, String>"
)]
#[case::inner_ref_non_slice_no_target(
    "inner_ref_non_slice_no_target",
    "#[garde(inner(length(chars, max = 8)))] value: &'a String"
)]
// Round 11: one `length` rule per mode; every rule's bounds apply.
#[case::length_per_mode(
    "length_per_mode",
    "#[garde(length(chars, min = 2, max = 10), length(bytes, max = 40))] value: String"
)]
#[case::length_per_mode_tightest(
    "length_per_mode_tightest",
    "#[garde(length(min = 1), length(chars, max = 10))] #[garde(length(chars, min = 3), length(utf16, max = 4))] value: String"
)]
#[case::length_per_mode_vec(
    "length_per_mode_vec",
    "#[garde(length(min = 1, max = 5), length(bytes, max = 2))] value: Vec<u8>"
)]
// Round 11: a Rust-only `regex` construct has no ECMA-262 reading.
#[case::pattern_rust_only_skipped(
    "pattern_rust_only_skipped",
    r#"#[garde(pattern(r"(?i)^abc$"), prefix("x"))] value: String"#
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
#[case::explicit_wins_inner_option(
    "explicit_wins_inner_option",
    r#"#[schema(pattern = "^[a-z]+$", format = "hostname")]
       #[garde(inner(prefix("ab"), ip))]
       value: Option<String>"#
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

#[rstest]
#[case::plain(r"^[A-Z]{2}\d+$", true)]
#[case::non_capturing(r"(?:ab)+", true)]
#[case::named_ecma(r"(?<year>\d{4})", true)]
#[case::lookahead(r"(?=a)(?!b)", true)]
#[case::unicode_class(r"\p{L}+", true)]
#[case::escaped_backslash(r"\\A", true)]
// Round 13: inside a class Rust nests `[..]` and reads `&&` / `--` / `~~`
// as set operations and a leading `]` as a literal; ECMA-262 does not.
#[case::nested_class(r"[[a]]", false)]
#[case::intersection(r"^[a-z&&[^aeiou]]+$", false)]
#[case::intersection_flat(r"[a-z&&b]", false)]
#[case::difference(r"[a-z--b]", false)]
#[case::symmetric_difference(r"[a-z~~b]", false)]
#[case::leading_bracket(r"[]a]", false)]
#[case::negated_leading_bracket(r"[^]a]", false)]
#[case::escaped_brackets_in_class(r"[\[\]]+", true)]
#[case::class_then_class(r"[a-][b]&&--~~", true)]
#[case::group_in_class(r"[(?i)]", true)]
#[case::unclosed_class(r"[a", true)]
#[case::trailing_backslash(r"a\", true)]
#[case::trailing_open_paren(r"a(", true)]
#[case::inline_flag(r"(?i)abc", false)]
#[case::flag_group(r"(?x: a b )", false)]
#[case::negated_flag(r"(?-i)abc", false)]
#[case::rust_named(r"(?P<y>\d)", false)]
#[case::start_of_text(r"\Aabc", false)]
#[case::end_of_text(r"abc\z", false)]
#[case::hex_brace(r"\x{41}", false)]
#[case::hex_plain(r"\x41", true)]
#[case::one_letter_property(r"\pL", false)]
#[case::posix_class(r"[[:alpha:]]", false)]
// Round 14: more Rust-only escapes, and a `}` that closes no `{`.
#[case::word_start(r"\<foo", false)]
#[case::word_end(r"foo\>", false)]
#[case::word_boundary_brace(r"\b{start}foo", false)]
#[case::word_boundary_plain(r"\bfoo\b", true)]
#[case::long_unicode_escape(r"\U0001F600", false)]
#[case::unicode_brace(r"\u{1F600}", false)]
#[case::unicode_plain(r"\u0041", true)]
#[case::bell(r"\a", false)]
#[case::lone_close_brace(r"a}", false)]
#[case::quantifier_braces(r"a{2,3}b{4}", true)]
#[case::close_brace_in_class(r"[}]", true)]
#[case::escaped_close_brace(r"a\}", true)]
// Round 15: escapes are an allowlist (Rust accepts any escaped punctuation,
// `u` mode only syntax characters, `/` and in a class `-`), a `]` outside a
// class, and `\p{..}` names ECMA-262 spells differently.
#[case::escaped_dash(r"^\d{3}\-\d{4}$", false)]
#[case::escaped_dash_in_class(r"[a\-z]", true)]
#[case::escaped_hash(r"\#", false)]
#[case::escaped_tilde(r"\~", false)]
#[case::escaped_colon(r"\:", false)]
#[case::escaped_quote(r#"\""#, false)]
#[case::escaped_underscore(r"\_", false)]
#[case::escaped_space(r"\ ", false)]
#[case::escaped_letter(r"\e", false)]
#[case::escaped_syntax(r"\^\$\\\.\*\+\?\(\)\[\]\{\}\|\/", true)]
#[case::control_escapes(r"\n\r\t\f\v\d\D\w\W\s\S\B", true)]
#[case::lone_close_bracket(r"a]", false)]
#[case::script_lone(r"\p{Greek}", false)]
#[case::script_keyed(r"\p{sc=Greek}\P{Script_Extensions=Old_Italic}", true)]
#[case::script_lowercase(r"\p{sc=greek}", false)]
#[case::script_colon(r"\p{sc:Greek}", false)]
#[case::category_long(r"[\p{Uppercase_Letter}\P{Nd}]", true)]
#[case::category_keyed(r"\p{gc=Lu}", true)]
#[case::category_keyed_unknown(r"\p{gc=Greek}", false)]
#[case::category_loose(r"\p{lu}", false)]
#[case::binary_property(r"\p{White_Space}\p{Alpha}", true)]
#[case::unknown_key(r"\p{Block=Basic_Latin}", false)]
#[case::unclosed_property(r"\p{L", true)]
// Round 18: Rust skips whitespace inside a counted repetition; ECMA-262 reads
// only `digits[,[digits]]` there.
#[case::brace_spaces(r"a{ 5 }", false)]
#[case::brace_space_after_comma(r"^\d{3, 4}$", false)]
#[case::brace_open_range(r"a{2,}", true)]
#[case::brace_leading_comma(r"a{,5}", false)]
#[case::brace_two_commas(r"a{1,2,3}", false)]
#[case::brace_letter(r"a{x}", false)]
fn ecma_compatible_patterns(#[case] pattern: &str, #[case] expected: bool) {
    assert_eq!(is_ecma_compatible(pattern), expected, "{pattern}");
}
