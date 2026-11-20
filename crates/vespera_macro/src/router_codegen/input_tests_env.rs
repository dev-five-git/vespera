use super::*;

/// Every `VESPERA_*` fallback treats a set-but-empty variable as unset: an
/// empty `VESPERA_DIR=` / `VESPERA_SERVER_URL=` no longer breaks the build,
/// and an empty `VESPERA_VERSION=` keeps the crate version.
#[test]
fn empty_vespera_env_fallbacks_are_unset() {
    let _dir = crate::env_access::test_env::set_var("VESPERA_DIR", "");
    let _openapi = crate::env_access::test_env::set_var("VESPERA_OPENAPI", "");
    let _title = crate::env_access::test_env::set_var("VESPERA_TITLE", "");
    let _version = crate::env_access::test_env::set_var("VESPERA_VERSION", "");
    let _url = crate::env_access::test_env::set_var("VESPERA_SERVER_URL", "");
    let _description = crate::env_access::test_env::set_var("VESPERA_SERVER_DESCRIPTION", "");
    let input = syn::parse2::<AutoRouterInput>(quote::quote!())
        .expect("empty VESPERA_* variables must be ignored");
    assert!(input.dir.is_none());
    assert!(input.openapi.is_none());
    assert!(input.title.is_none());
    assert_eq!(
        input.version.map(|version| version.value()).as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(input.servers.is_none());
}

/// A server URL from the environment with an empty
/// `VESPERA_SERVER_DESCRIPTION=` keeps no description.
#[test]
fn empty_server_description_env_is_unset() {
    let _url =
        crate::env_access::test_env::set_var("VESPERA_SERVER_URL", "https://api.example.com");
    let _description = crate::env_access::test_env::set_var("VESPERA_SERVER_DESCRIPTION", "");
    let input = syn::parse2::<AutoRouterInput>(quote::quote!())
        .expect("a valid server URL from the environment");
    let servers = input.servers.expect("the env server");
    assert!(servers[0].description.is_none());
}

/// An invalid `VESPERA_DIR` is reported with the variable it came from, since
/// the error points at the macro call (an argument `dir` is not named).
#[test]
fn invalid_vespera_dir_env_error_names_the_variable() {
    let _dir = crate::env_access::test_env::set_var("VESPERA_DIR", "../escape");
    let error = syn::parse2::<AutoRouterInput>(quote::quote!())
        .err()
        .expect("an invalid route folder must be rejected");
    assert!(
        error
            .to_string()
            .contains("(from the `VESPERA_DIR` environment variable)"),
        "{error}"
    );
}

#[cfg(unix)]
fn non_unicode() -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(vec![0x66, 0x80])
}

#[cfg(windows)]
fn non_unicode() -> std::ffi::OsString {
    use std::os::windows::ffi::OsStringExt;
    std::ffi::OsString::from_wide(&[0x66, 0xD800])
}

/// A set but non-Unicode `VESPERA_*` fallback is a compile error naming the
/// variable, like a malformed one: it is never silently ignored.
#[test]
fn non_unicode_vespera_env_is_a_compile_error_naming_it() {
    let _title = crate::env_access::test_env::set_var("VESPERA_TITLE", non_unicode());
    let Err(error) = syn::parse2::<AutoRouterInput>(quote::quote!()) else {
        panic!("a non-Unicode fallback must be rejected");
    };
    assert!(error.to_string().contains("VESPERA_TITLE"), "{error}");
}
