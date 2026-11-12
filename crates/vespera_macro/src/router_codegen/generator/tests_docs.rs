//! Generated tokens for the documentation routes (`docs_url`, `redoc_url`,
//! `spec_url`).

use rstest::rstest;

use super::*;

const SPEC: &str = r#"{"openapi":"3.1.0","info":{"title":"x","description":"</script>"}}"#;

fn render(
    docs_url: Option<&str>,
    redoc_url: Option<&str>,
    spec_url: Option<&str>,
    merge_apps: &[syn::Path],
) -> String {
    let code = generate_router_code(
        &CollectedMetadata::new(),
        docs_url,
        redoc_url,
        spec_url,
        Some(quote!(#SPEC)),
        merge_apps,
        &[],
    );
    let file: syn::File = syn::parse2(quote!(fn create_app() { #code })).expect("valid fn");
    prettyplease::unparse(&file)
}

#[rstest]
#[case::all_routes(
    "docs_routes_all_three",
    Some("/docs"),
    Some("/redoc"),
    Some("/openapi.json"),
    false
)]
#[case::spec_only("docs_routes_spec_url_only", None, None, Some("/openapi.json"), false)]
#[case::spec_with_merge(
    "docs_routes_spec_url_with_merge",
    None,
    None,
    Some("/openapi.json"),
    true
)]
fn docs_route_tokens_snapshot(
    #[case] name: &str,
    #[case] docs_url: Option<&str>,
    #[case] redoc_url: Option<&str>,
    #[case] spec_url: Option<&str>,
    #[case] merge: bool,
) {
    let merge_apps: Vec<syn::Path> = if merge {
        vec![syn::parse_quote!(third::ThirdApp)]
    } else {
        Vec::new()
    };
    insta::assert_snapshot!(name, render(docs_url, redoc_url, spec_url, &merge_apps));
}

/// The pages are rendered by the runtime `vespera::docs` helpers (which pin
/// and SRI-check the assets and escape the spec for `<script>`), never by
/// splicing the raw spec into an HTML template at the macro call site.
#[test]
fn docs_pages_render_through_the_escaping_runtime_helpers() {
    let code = render(Some("/docs"), Some("/redoc"), None, &[]);
    assert!(
        code.contains("vespera::docs::swagger_ui_html(__VESPERA_SPEC)"),
        "{code}"
    );
    assert!(
        code.contains("vespera::docs::redoc_html(__VESPERA_SPEC)"),
        "{code}"
    );
    assert!(!code.contains("<script"), "{code}");
}

/// Without any docs route the spec constant is not emitted at all.
#[test]
fn no_docs_routes_means_no_spec_constant() {
    let code = render(None, None, None, &[]);
    assert!(!code.contains("__VESPERA_SPEC"), "{code}");
}
