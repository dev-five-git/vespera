//! `ValidatedWith<C, T>` derives its `Clone` / `Copy` / `Debug` from the
//! wrapped payload only: the context type `C` is a `PhantomData` marker and
//! must not need any of those traits itself (an app context that holds a
//! pool is typically none of them).

#![cfg(feature = "validation")]

use ::vespera::ValidatedWith;

/// Implements none of `Clone`, `Copy`, `Debug`.
struct AppContext {
    _pool: Vec<u8>,
}

#[test]
fn clone_and_debug_need_only_the_payload() {
    let wrapped: ValidatedWith<AppContext, String> = ValidatedWith::new("x".to_owned());
    let cloned = wrapped.clone();
    assert_eq!(cloned.get(), "x");
    assert_eq!(format!("{wrapped:?}"), r#"ValidatedWith("x")"#);
}

#[test]
fn copy_needs_only_the_payload() {
    fn assert_copy<T: Copy>(_: &T) {}

    let wrapped: ValidatedWith<AppContext, u8> = ValidatedWith::new(7);
    assert_copy(&wrapped);
    let copied = wrapped;
    // `wrapped` is still usable after the move: the wrapper is `Copy`.
    assert_eq!((*copied.get(), *wrapped.get()), (7, 7));
}
