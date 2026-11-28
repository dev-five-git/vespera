//! Outward rounding of integer literal bounds past 2^53 (`outward_bound`): an
//! inexact literal never tightens an inclusive `maximum` / `minimum` (sidecar
//! of `schema_attrs`; `use super::*` semantics).

use rstest::rstest;
use syn::parse_quote;

use super::*;

/// Round 44: `9007199254740993` is halfway between two `f64`s and rounds to
/// `9007199254740992`, which is below it: a maximum goes up to the next float,
/// a minimum that rounded up goes down, and an exact value stays.
#[rstest]
#[case::maximum_rounded_down_goes_up(
    "9007199254740993",
    9_007_199_254_740_992.0,
    true,
    9_007_199_254_740_994.0
)]
#[case::minimum_rounded_down_is_looser(
    "9007199254740993",
    9_007_199_254_740_992.0,
    false,
    9_007_199_254_740_992.0
)]
#[case::negative_minimum_rounded_up_goes_down("-9007199254740993", -9_007_199_254_740_992.0, false, -9_007_199_254_740_994.0)]
#[case::negative_maximum_rounded_up_is_looser("-9007199254740993", -9_007_199_254_740_992.0, true, -9_007_199_254_740_992.0)]
#[case::exact_integer_unchanged(
    "9007199254740992",
    9_007_199_254_740_992.0,
    true,
    9_007_199_254_740_992.0
)]
#[case::small_integer_unchanged("5", 5.0, false, 5.0)]
#[case::float_literal_unchanged("1.5", 1.5, true, 1.5)]
#[case::negated_float_unchanged("-2.5", -2.5, false, -2.5)]
fn outward_bound_never_tightens_an_inexact_literal(
    #[case] literal: &str,
    #[case] value: f64,
    #[case] upper: bool,
    #[case] expected: f64,
) {
    let expr: Expr = syn::parse_str(literal).unwrap();
    assert_eq!(outward_bound(&expr, value, upper), expected, "{literal}");
}

/// `#[schema(maximum = ..)]` / `minimum = ..` take the same outward rounding.
#[test]
fn schema_maximum_and_minimum_round_outward() {
    let item: syn::ItemStruct = parse_quote! {
        struct Bounded {
            #[schema(maximum = 9007199254740993, minimum = -9007199254740993)]
            value: i64,
        }
    };
    let field = item.fields.iter().next().expect("one field");
    let constraints = extract_schema_constraints(&field.attrs);
    assert_eq!(constraints.maximum, Some(9_007_199_254_740_994.0));
    assert_eq!(constraints.minimum, Some(-9_007_199_254_740_994.0));
}
