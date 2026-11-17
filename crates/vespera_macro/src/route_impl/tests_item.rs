//! `#[route]` on an item that is not a plain function: the error names the
//! kind of item (a `fn` pointer type is not a function declaration), while a
//! function with a bad signature keeps syn's own message.

use proc_macro2::TokenStream;
use quote::quote;
use rstest::rstest;

use super::*;

#[rstest]
#[case::fn_pointer_alias(quote!(type Handler = fn(u8) -> u8;), false)]
#[case::static_fn_pointer(quote!(static CALLBACK: fn() = cb;), false)]
#[case::leading_group(quote!((a) fn broken() {}), false)]
#[case::leading_punct(quote!(- fn broken() {}), false)]
#[case::only_qualifiers(quote!(pub async), false)]
#[case::pub_crate_async_fn(quote!(pub(crate) async fn broken(a: u8 b: u8) {}), true)]
#[case::attributed_extern_fn(quote!(#[inline] extern "C" fn broken(a: u8 b: u8) {}), true)]
fn route_item_error_names_the_kind_of_item(#[case] item: TokenStream, #[case] is_fn: bool) {
    let err = process_route_attribute(quote!(get), item)
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("#[route] attribute: "), "{err}");
    assert_eq!(
        err.contains("can only be applied to functions"),
        !is_fn,
        "{err}"
    );
}
