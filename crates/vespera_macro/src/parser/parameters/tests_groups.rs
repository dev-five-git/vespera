//! Round 44: a `$t:ty` fragment (`Type::Group`) in a generic argument of an
//! extractor (`Query<$t>`, `Path<$t>`, `TypedHeader<$t>`) or in a flattened
//! `Option<$t>` reads as the type it wraps, as the written type does (sidecar
//! of `parameters`; `use super::*` semantics).

use std::collections::{HashMap, HashSet};

use rstest::rstest;
use syn::{FnArg, Type, parse_quote};

use super::*;

/// `ty` wrapped the way `macro_rules!` wraps a `$t:ty` fragment.
fn invisible(ty: &str) -> Type {
    Type::Group(syn::TypeGroup {
        attrs: Vec::new(),
        group_token: syn::token::Group::default(),
        elem: Box::new(syn::parse_str(ty).unwrap()),
    })
}

/// The parameters `arg` documents under `path_params`, as the full record.
fn documented(arg: &FnArg, path_params: &[&str], defs: &HashMap<&str, &str>) -> Vec<Parameter> {
    let path_params: Vec<String> = path_params.iter().map(|name| (*name).to_string()).collect();
    let path_set: HashSet<&str> = path_params.iter().map(String::as_str).collect();
    let known: HashSet<&str> = defs.keys().copied().collect();
    parse_function_parameter(arg, &path_params, &path_set, &known, defs).unwrap_or_default()
}

#[test]
fn query_of_a_group_documents_the_struct_fields() {
    let defs = HashMap::from([("Filter", "pub struct Filter { pub page: u32 }")]);
    let grouped = invisible("Filter");
    let arg: FnArg = parse_quote!(q: Query<#grouped>);
    let names: Vec<String> = documented(&arg, &[], &defs)
        .into_iter()
        .map(|parameter| parameter.name)
        .collect();
    assert_eq!(names, ["page"]);
}

#[test]
fn path_of_a_grouped_tuple_documents_each_segment_type() {
    let defs = HashMap::new();
    let grouped = invisible("(u64, String)");
    let arg: FnArg = parse_quote!(p: Path<#grouped>);
    let parameters = documented(&arg, &["a", "b"], &defs);
    let names: Vec<&str> = parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    assert_eq!(names, ["a", "b"]);
    let first = serde_json::to_value(&parameters[0]).unwrap();
    assert_eq!(first["schema"]["type"], "integer", "{first}");
}

#[test]
fn typed_header_of_a_group_is_named_after_the_type() {
    let defs = HashMap::new();
    let grouped = invisible("XRequestId");
    let arg: FnArg = parse_quote!(id: TypedHeader<#grouped>);
    let names: Vec<String> = documented(&arg, &[], &defs)
        .into_iter()
        .map(|parameter| parameter.name)
        .collect();
    assert_eq!(names, ["x-request-id"]);
}

#[test]
fn flattened_option_of_a_group_documents_the_struct_fields() {
    let grouped = invisible("Filter");
    let outer: syn::ItemStruct = parse_quote! {
        pub struct Outer {
            #[serde(flatten)]
            pub inner: Option<#grouped>,
        }
    };
    let outer_src = quote::quote!(#outer).to_string();
    let defs = HashMap::from([
        ("Outer", outer_src.as_str()),
        ("Filter", "pub struct Filter { pub page: u32 }"),
    ]);
    let arg: FnArg = parse_quote!(q: Query<Outer>);
    let names: Vec<String> = documented(&arg, &[], &defs)
        .into_iter()
        .map(|parameter| parameter.name)
        .collect();
    assert_eq!(names, ["page"]);
}

/// An `Option<..>` of a non-path type (a tuple, a const) is no `TypedHeader`,
/// through a group or not.
#[rstest]
#[case::tuple("Option<(u8, u8)>")]
#[case::const_argument("Option<3>")]
fn typed_header_option_of_a_non_path_documents_nothing(#[case] written: &str) {
    let ty: Type = syn::parse_str(written).unwrap();
    assert!(
        super::header::parse_typed_header(&ty).is_none(),
        "{written}"
    );
}
