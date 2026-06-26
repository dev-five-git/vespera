use super::*;
use syn::parse_quote;

#[allow(clippy::needless_pass_by_value)] // test helper takes owned input by convention
fn emit_to_string(input: DeriveInput) -> String {
    let constraints = constraints_for(&input);
    emit_garde_validate(&input, &constraints).to_string()
}

/// Recompute the per-field `SchemaConstraints` slice the way
/// `process_derive_schema` does in production code, so the tests can
/// drive `emit_garde_validate` (which now takes the slice as a
/// parameter) without re-stating the parse pass at every call site.
fn constraints_for(input: &DeriveInput) -> Vec<SchemaConstraints> {
    use crate::parser::schema::schema_attrs::try_extract_schema_constraints;
    if let Data::Struct(d) = &input.data
        && let Fields::Named(f) = &d.fields
    {
        return f
            .named
            .iter()
            .map(|fld| try_extract_schema_constraints(&fld.attrs).unwrap_or_default())
            .collect();
    }
    Vec::new()
}

#[test]
fn no_constraints_emits_nothing() {
    let s: DeriveInput = parse_quote! {
        struct User {
            pub name: String,
            pub age: i32,
        }
    };
    assert!(emit_to_string(s).is_empty());
}

#[test]
fn min_length_only_emits_length_chars_apply() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(min_length = 3)]
            pub name: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("impl :: vespera :: __validation :: garde :: Validate for User"));
    assert!(out.contains("length :: chars :: apply"));
    assert!(out.contains("3usize") || out.contains("3 usize"));
}

#[test]
fn min_and_max_length_combined_in_single_call() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(min_length = 3, max_length = 32)]
            pub name: String,
        }
    };
    let out = emit_to_string(s);
    // single length::chars::apply call carrying both bounds
    let occurrences = out.matches("length :: chars :: apply").count();
    assert_eq!(occurrences, 1);
}

#[test]
fn invalid_pattern_emits_compile_error_not_runtime_panic() {
    // An unbalanced group is a regex SYNTAX error: it must be caught at
    // macro expansion (compile_error!), not deferred to a runtime panic.
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(pattern = "(")]
            pub name: String,
        }
    };
    let out = emit_to_string(s);
    assert!(
        out.contains("compile_error"),
        "invalid pattern should emit compile_error, got: {out}"
    );
    assert!(
        !out.contains("LazyLock"),
        "invalid pattern must not emit a runtime regex validator: {out}"
    );
}

#[test]
fn valid_pattern_emits_regex_validator() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(pattern = "^[a-z0-9_]+$")]
            pub name: String,
        }
    };
    let out = emit_to_string(s);
    assert!(
        out.contains("LazyLock"),
        "valid pattern should emit a regex validator: {out}"
    );
    assert!(out.contains("pattern :: apply"));
    assert!(!out.contains("compile_error"));
}

#[test]
fn range_emit_uses_field_numeric_type() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(minimum = 0, maximum = 150)]
            pub age: u32,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("range :: apply"));
    assert!(out.contains("as u32"));
}

#[test]
fn range_emit_on_float_field_keeps_decimal_point() {
    let s: DeriveInput = parse_quote! {
        struct Price {
            #[schema(minimum = 0.01, maximum = 99.99)]
            pub amount: f64,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("range :: apply"));
    assert!(out.contains("as f64"));
}

#[test]
fn pattern_emits_static_lazy_lock_regex() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(pattern = "^[a-z]+$")]
            pub username: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("static __VESPERA_PATTERN_USERNAME"));
    assert!(out.contains("LazyLock"));
    assert!(out.contains("regex :: Regex :: new"));
    assert!(out.contains("pattern :: apply"));
}

#[test]
fn format_email_emits_email_apply() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(format = "email")]
            pub email: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("email :: apply"));
}

#[test]
fn format_uri_emits_url_apply() {
    let s: DeriveInput = parse_quote! {
        struct Site {
            #[schema(format = "uri")]
            pub home: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("url :: apply"));
}

#[test]
fn format_ipv4_emits_ip_apply_with_v4_kind() {
    let s: DeriveInput = parse_quote! {
        struct Host {
            #[schema(format = "ipv4")]
            pub addr: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("ip :: apply"));
    assert!(out.contains("IpKind :: V4"));
}

#[test]
fn format_uuid_is_annotation_only_no_runtime_rule() {
    let s: DeriveInput = parse_quote! {
        struct Entity {
            #[schema(format = "uuid")]
            pub id: String,
        }
    };
    // uuid alone has no garde rule → no Validate impl emitted.
    assert!(emit_to_string(s).is_empty());
}

#[test]
fn option_field_wraps_rule_block_in_if_let_some() {
    let s: DeriveInput = parse_quote! {
        struct User {
            #[schema(min_length = 3)]
            pub nickname: Option<String>,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("if let :: std :: option :: Option :: Some"));
    assert!(out.contains("length :: chars :: apply"));
}

#[test]
fn min_max_items_on_vec_emits_length_simple() {
    let s: DeriveInput = parse_quote! {
        struct Post {
            #[schema(min_items = 1, max_items = 5)]
            pub tags: Vec<String>,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("length :: simple :: apply"));
}

#[test]
fn enum_emits_nothing() {
    let e: DeriveInput = parse_quote! {
        enum Status { Active, Inactive }
    };
    assert!(emit_to_string(e).is_empty());
}

#[test]
fn tuple_struct_emits_nothing() {
    let s: DeriveInput = parse_quote! {
        struct Wrapper(pub String);
    };
    assert!(emit_to_string(s).is_empty());
}

#[test]
fn unit_struct_emits_nothing() {
    let s: DeriveInput = parse_quote! {
        struct Empty;
    };
    assert!(emit_to_string(s).is_empty());
}

#[test]
fn generic_struct_with_constraints_produces_compile_error() {
    let s: DeriveInput = parse_quote! {
        struct Wrapper<T> {
            #[schema(min_length = 3)]
            pub name: String,
            pub inner: T,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("compile_error"));
    assert!(out.contains("generic"));
}

#[test]
fn annotation_only_constraints_emit_nothing() {
    // example / read_only / write_only / unique_items / multiple_of /
    // exclusive bounds are OpenAPI annotations only; they should not
    // drag a Validate impl into existence on their own.
    let s: DeriveInput = parse_quote! {
        struct Doc {
            #[schema(read_only, example = "abc", unique_items, multiple_of = 0.5)]
            pub id: String,
        }
    };
    assert!(emit_to_string(s).is_empty());
}

// ── nested validation (`#[schema(dive)]`) emission ──────────────

#[test]
fn dive_on_plain_field_emits_validate_into_call() {
    let s: DeriveInput = parse_quote! {
        struct Order {
            #[schema(dive)]
            pub address: Address,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("impl :: vespera :: __validation :: garde :: Validate for Order"));
    assert!(out.contains("Validate :: validate_into"));
    assert!(out.contains("\"address\""));
}

#[test]
fn dive_on_option_wraps_in_if_let_some() {
    let s: DeriveInput = parse_quote! {
        struct Order {
            #[schema(dive)]
            pub address: Option<Address>,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("if let :: std :: option :: Option :: Some"));
    assert!(out.contains("Validate :: validate_into"));
}

#[test]
fn dive_on_vec_emits_single_validate_into_call() {
    // garde's runtime `Vec<T>: Validate` impl iterates and pushes
    // `[idx]` path components automatically — the macro only emits
    // one `validate_into` call regardless of container kind.
    let s: DeriveInput = parse_quote! {
        struct Order {
            #[schema(dive)]
            pub items: Vec<LineItem>,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("Validate :: validate_into"));
    // `validate_into` appears twice: once as the outer fn declaration
    // (`fn validate_into(...)`) and once as the inner trait dispatch
    // (`Validate :: validate_into(...)`).  Anything more would mean
    // the macro is iterating itself, which is what we explicitly
    // delegate to garde's runtime `Vec<T>: Validate` impl.
    assert_eq!(
        out.matches("validate_into").count(),
        2,
        "expected outer fn + one inner trait call; iteration is garde-runtime, \
             so the macro must NOT emit a `for` loop"
    );
    // `for` keyword appears in `impl ... for Order` — count only
    // tokens that look like loop iteration (`for <ident> in `).
    let loop_count = out.matches("in __garde_binding").count();
    assert_eq!(loop_count, 0, "macro must not emit explicit iteration");
}

#[test]
fn dive_combined_with_length_emits_both_rules() {
    let s: DeriveInput = parse_quote! {
        struct Order {
            #[schema(min_items = 1, max_items = 10, dive)]
            pub items: Vec<LineItem>,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("length :: simple :: apply"));
    assert!(out.contains("Validate :: validate_into"));
}

#[test]
fn dive_false_disables_emission() {
    let s: DeriveInput = parse_quote! {
        struct Order {
            #[schema(dive = false)]
            pub address: Address,
        }
    };
    // `dive = false` is the same as no annotation — no rule
    // produced means no `impl Validate` emitted.
    assert!(emit_to_string(s).is_empty());
}

// ── format=ipv6 / format=ip / unknown format ────────────────────

#[test]
fn format_ipv6_emits_ip_apply_with_v6_kind() {
    let s: DeriveInput = parse_quote! {
        struct Host {
            #[schema(format = "ipv6")]
            pub addr: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("ip :: apply"));
    assert!(out.contains("IpKind :: V6"));
}

#[test]
fn format_ip_emits_ip_apply_with_any_kind() {
    let s: DeriveInput = parse_quote! {
        struct Host {
            #[schema(format = "ip")]
            pub addr: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("ip :: apply"));
    assert!(out.contains("IpKind :: Any"));
}

#[test]
fn format_url_alias_emits_url_apply() {
    // `format = "url"` is the documented alias for `"uri"` —
    // both must dispatch to garde's `url::apply`.
    let s: DeriveInput = parse_quote! {
        struct Site {
            #[schema(format = "url")]
            pub home: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("url :: apply"));
}

#[test]
fn unknown_format_with_other_rule_skips_format_branch() {
    // Combining an unsupported `format = "custom"` with a known
    // runtime rule (`min_length = 3`) forces the emitter to enter
    // `emit_rule_blocks` AND fall through the unknown-format
    // branch — exercising the `_ => {}` arm.
    let s: DeriveInput = parse_quote! {
        struct Doc {
            #[schema(min_length = 3, format = "custom-thing")]
            pub id: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("length :: chars :: apply"));
    // The unknown format MUST NOT produce any `ip::`/`email::`/
    // `url::` call — confirms the `_ => {}` arm took effect.
    assert!(!out.contains("ip :: apply"));
    assert!(!out.contains("email :: apply"));
    assert!(!out.contains("url :: apply"));
}

// ── mixed-field structs exercising the no-runtime-rule early exit
//    inside emit_field_block ────────────────────────────────────

#[test]
fn mixed_validated_and_unvalidated_fields_emit_only_validated_blocks() {
    // `a` has a runtime rule; `b` does not.  emit_field_block must
    // hit its early `return None` for `b` while still emitting `a`.
    let s: DeriveInput = parse_quote! {
        struct Mixed {
            #[schema(min_length = 3)]
            pub a: String,
            pub b: String,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("impl :: vespera :: __validation :: garde :: Validate for Mixed"));
    assert!(out.contains("\"a\""));
    // Field `b` has no constraint — no path literal should appear.
    assert!(!out.contains("\"b\""));
}

// ── one-sided numeric bounds exercising numeric_some(None, _) ───

#[test]
fn only_minimum_set_emits_none_for_max_bound() {
    let s: DeriveInput = parse_quote! {
        struct N {
            #[schema(minimum = 0)]
            pub n: u32,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("range :: apply"));
    // The missing upper bound must serialize as Option::None.
    assert!(out.contains("Option :: None"));
}

#[test]
fn only_maximum_set_emits_none_for_min_bound() {
    let s: DeriveInput = parse_quote! {
        struct N {
            #[schema(maximum = 100)]
            pub n: u32,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("range :: apply"));
    assert!(out.contains("Option :: None"));
}

// ── numeric_some with unknown numeric_kind (non-primitive field) ─

#[test]
fn minimum_on_non_primitive_field_falls_back_to_as_wildcard() {
    // Field type is a user-defined `Money` newtype — peel_option
    // returns None and rust_numeric_kind returns None, forcing
    // numeric_some down the `as _` fallback branch.
    let s: DeriveInput = parse_quote! {
        struct Order {
            #[schema(minimum = 0)]
            pub price: Money,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("range :: apply"));
    assert!(
        out.contains("as _"),
        "non-primitive field should emit `as _` fallback, got: {out}"
    );
}

#[test]
fn tuple_typed_field_does_not_trip_option_or_numeric_helpers() {
    let s: DeriveInput = parse_quote! {
        struct WithTuple {
            #[schema(min_length = 3)]
            pub x: (String,),
        }
    };
    let out = emit_to_string(s);
    assert!(!out.contains("if let :: std :: option :: Option :: Some"));
    assert!(out.contains("length :: chars :: apply"));
}

#[test]
fn bare_option_without_angle_brackets_falls_through_peel() {
    let s: DeriveInput = parse_quote! {
        struct BareOption {
            #[schema(min_length = 3)]
            pub x: Option,
        }
    };
    let out = emit_to_string(s);
    assert!(!out.contains("if let :: std :: option :: Option :: Some"));
    assert!(out.contains("length :: chars :: apply"));
}

#[test]
fn option_with_lifetime_only_arg_falls_through_find_map() {
    let s: DeriveInput = parse_quote! {
        struct WithLifetime {
            #[schema(min_length = 3)]
            pub x: Option<'static>,
        }
    };
    let out = emit_to_string(s);
    assert!(out.contains("length :: chars :: apply"));
}
