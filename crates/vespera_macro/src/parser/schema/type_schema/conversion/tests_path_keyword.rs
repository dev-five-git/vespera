//! `crate::Schema` / `self::Schema` / `super::Schema` (sidecar of
//! `conversion`; `use super::*` semantics preserved): a path keyword is no
//! module name, so the `<module>Schema` convention never picks a component
//! named after the keyword.

use rstest::rstest;

use super::*;

/// The bare `Schema` and a component named `CrateSchema` (`SelfSchema` /
/// `SuperSchema`) are both known; a keyword-qualified `Schema` names the bare
/// one, never the convention's `<Keyword>Schema` component.
#[rstest]
#[case::crate_root("crate::Schema", "CrateSchema")]
#[case::self_module("self::Schema", "SelfSchema")]
#[case::super_module("super::Schema", "SuperSchema")]
fn path_keyword_parent_names_the_bare_schema(#[case] ty_src: &str, #[case] other: &str) {
    let known = HashSet::from(["Schema", other]);
    let defs: HashMap<String, String> = HashMap::new();
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    let SchemaRef::Ref(reference) = schema_ref else {
        panic!("a $ref for {ty_src}: {schema_ref:?}");
    };
    assert_eq!(reference.ref_path, "#/components/schemas/Schema");
}
