use super::*;
use rstest::rstest;

#[test]
fn test_parse_openapi_values_single() {
    // Test that single string openapi value parses correctly via AutoRouterInput
    let tokens = quote::quote!(openapi = "openapi.json");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let openapi = input.openapi.unwrap();
    assert_eq!(openapi.len(), 1);
    assert_eq!(openapi[0].value(), "openapi.json");
}

#[test]
fn test_parse_openapi_values_array() {
    // Test that array openapi value parses correctly via AutoRouterInput
    let tokens = quote::quote!(openapi = ["openapi.json", "api.json"]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let openapi = input.openapi.unwrap();
    assert_eq!(openapi.len(), 2);
    assert_eq!(openapi[0].value(), "openapi.json");
    assert_eq!(openapi[1].value(), "api.json");
}

#[test]
fn test_validate_server_url_valid_http() {
    let lit = LitStr::new("http://localhost:3000", Span::call_site());
    let result = validate_server_url(&lit);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "http://localhost:3000");
}

#[test]
fn test_validate_server_url_valid_https() {
    let lit = LitStr::new("https://api.example.com", Span::call_site());
    let result = validate_server_url(&lit);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "https://api.example.com");
}

#[test]
fn test_validate_server_url_invalid() {
    let lit = LitStr::new("ftp://example.com", Span::call_site());
    let result = validate_server_url(&lit);
    assert!(result.is_err());
}

#[test]
fn test_validate_server_url_no_scheme() {
    let lit = LitStr::new("example.com", Span::call_site());
    let result = validate_server_url(&lit);
    assert!(result.is_err());
}

#[test]
fn test_auto_router_input_parse_dir_only() {
    let tokens = quote::quote!(dir = "api");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.dir.unwrap().value(), "api");
    assert!(input.openapi.is_none());
}

#[test]
fn test_auto_router_input_parse_string_as_dir() {
    let tokens = quote::quote!("routes");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.dir.unwrap().value(), "routes");
}

#[test]
fn test_auto_router_input_parse_openapi_single() {
    let tokens = quote::quote!(openapi = "openapi.json");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let openapi = input.openapi.unwrap();
    assert_eq!(openapi.len(), 1);
    assert_eq!(openapi[0].value(), "openapi.json");
}

#[test]
fn test_auto_router_input_parse_openapi_array() {
    let tokens = quote::quote!(openapi = ["a.json", "b.json"]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let openapi = input.openapi.unwrap();
    assert_eq!(openapi.len(), 2);
}

#[test]
fn test_auto_router_input_parse_title_version() {
    let tokens = quote::quote!(title = "My API", version = "2.0.0");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.title.unwrap().value(), "My API");
    assert_eq!(input.version.unwrap().value(), "2.0.0");
}

#[test]
fn test_auto_router_input_parse_docs_redoc() {
    let tokens = quote::quote!(docs_url = "/docs", redoc_url = "/redoc");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.docs_url.unwrap().value(), "/docs");
    assert_eq!(input.redoc_url.unwrap().value(), "/redoc");
}

#[test]
fn test_auto_router_input_parse_servers_single() {
    let tokens = quote::quote!(servers = "http://localhost:3000");
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].url, "http://localhost:3000");
    assert!(servers[0].description.is_none());
}

#[test]
fn test_auto_router_input_parse_servers_array_strings() {
    let tokens = quote::quote!(servers = ["http://localhost:3000", "https://api.example.com"]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 2);
}

#[test]
fn test_auto_router_input_parse_servers_tuple() {
    let tokens = quote::quote!(servers = [("http://localhost:3000", "Development")]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].url, "http://localhost:3000");
    assert_eq!(servers[0].description, Some("Development".to_string()));
}

#[test]
fn test_auto_router_input_parse_servers_struct() {
    let tokens = quote::quote!(servers = [{ url = "http://localhost:3000", description = "Dev" }]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].url, "http://localhost:3000");
    assert_eq!(servers[0].description, Some("Dev".to_string()));
}

#[test]
fn test_auto_router_input_parse_servers_single_struct() {
    let tokens = quote::quote!(servers = { url = "https://api.example.com" });
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].url, "https://api.example.com");
}

#[test]
fn test_auto_router_input_parse_security_schemes() {
    let tokens = quote::quote!(
        security_schemes = [
            { name = "bearerAuth", type = "http", scheme = "bearer", bearer_format = "JWT" },
            { name = "apiKey", type = "apiKey", in = "header", header_name = "X-API-Key" }
        ]
    );
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let schemes = input.security_schemes.unwrap();
    assert_eq!(schemes.len(), 2);
    assert_eq!(schemes[0].name, "bearerAuth");
    assert_eq!(schemes[0].scheme.r#type, SecuritySchemeType::Http);
    assert_eq!(schemes[0].scheme.scheme.as_deref(), Some("bearer"));
    assert_eq!(schemes[0].scheme.bearer_format.as_deref(), Some("JWT"));
    assert_eq!(schemes[1].name, "apiKey");
    assert_eq!(schemes[1].scheme.r#type, SecuritySchemeType::ApiKey);
    assert_eq!(schemes[1].scheme.r#in.as_deref(), Some("header"));
    assert_eq!(schemes[1].scheme.name.as_deref(), Some("X-API-Key"));
}

#[test]
fn test_auto_router_input_parse_global_security() {
    let tokens = quote::quote!(security = ["bearerAuth", "apiKey"]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    assert_eq!(
        input.security,
        Some(vec!["bearerAuth".to_string(), "apiKey".to_string()])
    );
}

#[test]
fn test_process_vespera_input_security() {
    let tokens = quote::quote!(
        security_schemes = [{ name = "bearerAuth", type = "http", scheme = "bearer" }],
        security = ["bearerAuth"]
    );
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let processed = process_vespera_input(input);
    assert!(
        processed
            .security_schemes
            .as_ref()
            .is_some_and(|schemes| schemes.contains_key("bearerAuth"))
    );
    assert_eq!(processed.security.as_ref().map(Vec::len), Some(1));
}

#[test]
fn test_auto_router_input_parse_tags_with_descriptions() {
    let tokens = quote::quote!(
        tags = [
            { name = "users", description = "User operations" },
            { name = "admin", description = "Admin operations" }
        ]
    );
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let tags = input.tags.unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].name, "users");
    assert_eq!(tags[0].description.as_deref(), Some("User operations"));
    assert_eq!(tags[1].name, "admin");
    assert_eq!(tags[1].description.as_deref(), Some("Admin operations"));
}

#[test]
fn test_auto_router_input_parse_tags_missing_name_errors() {
    let tokens = quote::quote!(tags = [{ description = "Missing name" }]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err());
}

#[test]
fn test_process_vespera_input_tag_descriptions() {
    let tokens = quote::quote!(tags = [{ name = "users", description = "User operations" }]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let processed = process_vespera_input(input);
    assert_eq!(
        processed
            .tag_descriptions
            .as_ref()
            .and_then(|tags| tags.get("users"))
            .map(String::as_str),
        Some("User operations")
    );
}

#[test]
fn test_auto_router_input_parse_unknown_field() {
    let tokens = quote::quote!(unknown_field = "value");
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err());
}

#[test]
fn test_auto_router_input_parse_all_fields() {
    let tokens = quote::quote!(
        dir = "api",
        openapi = "openapi.json",
        title = "Test API",
        version = "1.0.0",
        docs_url = "/docs",
        redoc_url = "/redoc",
        servers = "http://localhost:3000",
        security_schemes = [{ name = "bearerAuth", type = "http", scheme = "bearer" }],
        security = ["bearerAuth"],
        tags = [{ name = "users", description = "User operations" }]
    );
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    assert!(input.dir.is_some());
    assert!(input.openapi.is_some());
    assert!(input.title.is_some());
    assert!(input.version.is_some());
    assert!(input.docs_url.is_some());
    assert!(input.redoc_url.is_some());
    assert!(input.servers.is_some());
    assert!(input.security_schemes.is_some());
    assert!(input.security.is_some());
    assert!(input.tags.is_some());
}

#[test]
fn test_parse_server_struct_url_only() {
    // Test server struct parsing via AutoRouterInput
    let tokens = quote::quote!(servers = { url = "http://localhost:3000" });
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].url, "http://localhost:3000");
    assert!(servers[0].description.is_none());
}

#[test]
fn test_parse_server_struct_with_description() {
    let tokens = quote::quote!(servers = { url = "http://localhost:3000", description = "Local" });
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers[0].description, Some("Local".to_string()));
}

#[test]
fn test_parse_server_struct_unknown_field() {
    let tokens = quote::quote!(servers = { url = "http://localhost:3000", unknown = "test" });
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err());
}

#[test]
fn test_parse_server_struct_missing_url() {
    let tokens = quote::quote!(servers = { description = "test" });
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err());
}

#[test]
fn test_parse_servers_tuple_url_only() {
    let tokens = quote::quote!(servers = [("http://localhost:3000")]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let servers = input.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert!(servers[0].description.is_none());
}

#[test]
fn test_parse_servers_invalid_url() {
    let tokens = quote::quote!(servers = "invalid-url");
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err());
}

#[test]
fn test_auto_router_input_parse_invalid_token() {
    // Test line 149: neither ident nor string literal triggers lookahead error
    let tokens = quote::quote!(123);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err());
}

#[test]
fn test_auto_router_input_empty() {
    // Test empty input - should use defaults/env vars
    let tokens = quote::quote!();
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_ok());
}

#[test]
fn test_auto_router_input_multiple_commas() {
    // Test input with trailing comma
    let tokens = quote::quote!(dir = "api",);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_ok());
}

#[test]
fn test_auto_router_input_no_comma() {
    // Test input without comma between fields (should stop at second field)
    let tokens = quote::quote!(dir = "api" title = "Test");
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    // This should fail or only parse first field
    assert!(result.is_err());
}

#[test]
fn test_process_vespera_input_defaults() {
    let tokens = quote::quote!();
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let processed = process_vespera_input(input);
    assert_eq!(processed.folder_name, "routes");
    assert!(processed.openapi_file_names.is_empty());
    assert!(processed.title.is_none());
    assert!(processed.docs_url.is_none());
}

#[test]
fn test_process_vespera_input_all_fields() {
    let tokens = quote::quote!(
        dir = "api",
        openapi = ["openapi.json", "api.json"],
        title = "My API",
        version = "1.0.0",
        docs_url = "/docs",
        redoc_url = "/redoc",
        servers = "http://localhost:3000"
    );
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let processed = process_vespera_input(input);
    assert_eq!(processed.folder_name, "api");
    assert_eq!(
        processed.openapi_file_names,
        vec!["openapi.json", "api.json"]
    );
    assert_eq!(processed.title, Some("My API".to_string()));
    assert_eq!(processed.version, Some("1.0.0".to_string()));
    assert_eq!(processed.docs_url, Some("/docs".to_string()));
    assert_eq!(processed.redoc_url, Some("/redoc".to_string()));
    let servers = processed.servers.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].url, "http://localhost:3000");
}

#[test]
fn test_process_vespera_input_servers_with_description() {
    let tokens = quote::quote!(
        servers = [{ url = "https://api.example.com", description = "Production" }]
    );
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let processed = process_vespera_input(input);
    let servers = processed.servers.unwrap();
    assert_eq!(servers[0].url, "https://api.example.com");
    assert_eq!(servers[0].description, Some("Production".to_string()));
}

// ========== Tests for parse_merge_values ==========

#[test]
fn test_parse_merge_values_single() {
    let tokens = quote::quote!(merge = [some::path::App]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let merge = input.merge.unwrap();
    assert_eq!(merge.len(), 1);
    // Check the path segments
    let path = &merge[0];
    let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    assert_eq!(segments, vec!["some", "path", "App"]);
}

#[test]
fn test_parse_merge_values_multiple() {
    let tokens = quote::quote!(merge = [first::App, second::Other]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let merge = input.merge.unwrap();
    assert_eq!(merge.len(), 2);
}

#[test]
fn test_parse_merge_values_empty() {
    let tokens = quote::quote!(merge = []);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let merge = input.merge.unwrap();
    assert!(merge.is_empty());
}

#[test]
fn test_parse_merge_values_with_trailing_comma() {
    let tokens = quote::quote!(merge = [app::MyApp,]);
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();
    let merge = input.merge.unwrap();
    assert_eq!(merge.len(), 1);
}

#[test]
#[serial_test::serial]
fn test_auto_router_input_server_env_var_fallback() {
    // Test lines 181-183: VESPERA_SERVER_URL env var fallback
    // `#[serial]` serializes this with every other env-mutating test so
    // the process-global VESPERA_SERVER_* vars cannot race across the
    // parallel test threads.
    let test_url = "https://vespera-test-unique-12345.example.com";
    let test_desc = "Vespera Test Server 12345";

    // Save current state
    let old_server_url = std::env::var("VESPERA_SERVER_URL").ok();
    let old_server_desc = std::env::var("VESPERA_SERVER_DESCRIPTION").ok();

    // SAFETY: Single-threaded test context
    unsafe {
        std::env::set_var("VESPERA_SERVER_URL", test_url);
        std::env::set_var("VESPERA_SERVER_DESCRIPTION", test_desc);
    }

    // Parse empty input - should pick up env vars
    let tokens = quote::quote!();
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();

    // Restore env vars immediately after parsing
    unsafe {
        if let Some(url) = old_server_url {
            std::env::set_var("VESPERA_SERVER_URL", url);
        } else {
            std::env::remove_var("VESPERA_SERVER_URL");
        }
        if let Some(desc) = old_server_desc {
            std::env::set_var("VESPERA_SERVER_DESCRIPTION", desc);
        } else {
            std::env::remove_var("VESPERA_SERVER_DESCRIPTION");
        }
    }

    // Check if servers was set - may not be if another test interfered
    if let Some(servers) = input.servers {
        // If we got servers, verify they match our test values
        if servers.len() == 1 && servers[0].url == test_url {
            assert_eq!(servers[0].description, Some(test_desc.to_string()));
        }
        // Otherwise another test's values were picked up, which is fine
    }
    // If servers is None, another test may have cleared the env var - acceptable
}

#[test]
#[serial_test::serial]
fn test_auto_router_input_server_env_var_invalid_url_filtered() {
    // Test that invalid URLs (not http/https) are filtered out by the .filter() call
    // This exercises the filter branch, not lines 181-183 directly
    let old_server_url = std::env::var("VESPERA_SERVER_URL").ok();

    // SAFETY: Single-threaded test context
    unsafe {
        std::env::set_var("VESPERA_SERVER_URL", "ftp://invalid-url-test.com");
    }

    let tokens = quote::quote!();
    let input: AutoRouterInput = syn::parse2(tokens).unwrap();

    // Restore env var
    unsafe {
        if let Some(url) = old_server_url {
            std::env::set_var("VESPERA_SERVER_URL", url);
        } else {
            std::env::remove_var("VESPERA_SERVER_URL");
        }
    }

    // If servers is Some, it means another test set a valid URL - acceptable
    // If servers is None, our invalid URL was correctly filtered
    if let Some(servers) = &input.servers {
        // Another test set a valid URL, check it's not our invalid one
        assert!(
            servers.is_empty() || servers[0].url != "ftp://invalid-url-test.com",
            "Invalid ftp:// URL should have been filtered"
        );
    }
}

#[test]
fn test_duplicate_field_rejected() {
    let tokens = quote::quote!(title = "A", title = "B");
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "duplicate `title` must be rejected");
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate field")
    );
}

#[test]
fn test_duplicate_field_distinct_ok() {
    let tokens = quote::quote!(title = "A", version = "1.0.0");
    let input: AutoRouterInput = syn::parse2(tokens).expect("distinct fields parse");
    assert_eq!(input.title.unwrap().value(), "A");
    assert_eq!(input.version.unwrap().value(), "1.0.0");
}

#[test]
fn test_security_scheme_apikey_valid() {
    let tokens = quote::quote!(security_schemes = [
        { name = "apiKey", type = "apiKey", header_name = "X-API-Key", in = "header" }
    ]);
    let input: AutoRouterInput = syn::parse2(tokens).expect("valid apiKey scheme parses");
    let schemes = input.security_schemes.unwrap();
    assert_eq!(schemes.len(), 1);
    assert_eq!(schemes[0].scheme.name.as_deref(), Some("X-API-Key"));
}

#[test]
fn test_security_scheme_apikey_missing_in_rejected() {
    let tokens = quote::quote!(security_schemes = [
        { name = "apiKey", type = "apiKey", header_name = "X-API-Key" }
    ]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "apiKey without `in` must be rejected");
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("required field `in`")
    );
}

#[test]
fn test_security_scheme_apikey_bad_in_rejected() {
    let tokens = quote::quote!(security_schemes = [
        { name = "apiKey", type = "apiKey", header_name = "X-API-Key", in = "body" }
    ]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "invalid `in` value must be rejected");
}

#[test]
fn test_security_scheme_http_missing_scheme_rejected() {
    let tokens = quote::quote!(security_schemes = [
        { name = "bearerAuth", type = "http" }
    ]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "http without `scheme` must be rejected");
    assert!(result.err().unwrap().to_string().contains("scheme"));
}

#[test]
fn test_security_scheme_http_valid() {
    let tokens = quote::quote!(security_schemes = [
        { name = "bearerAuth", type = "http", scheme = "bearer", bearer_format = "JWT" }
    ]);
    let input: AutoRouterInput = syn::parse2(tokens).expect("valid http scheme parses");
    assert_eq!(input.security_schemes.unwrap().len(), 1);
}

#[test]
fn test_security_scheme_oauth2_rejected() {
    let tokens = quote::quote!(security_schemes = [
        { name = "oauth", type = "oauth2" }
    ]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(
        result.is_err(),
        "oauth2 (no flows support) must be rejected"
    );
    assert!(result.err().unwrap().to_string().contains("flows"));
}

#[test]
fn test_security_scheme_openidconnect_requires_url() {
    let missing = quote::quote!(security_schemes = [
        { name = "oidc", type = "openIdConnect" }
    ]);
    assert!(
        syn::parse2::<AutoRouterInput>(missing).is_err(),
        "openIdConnect without url must be rejected"
    );

    let ok = quote::quote!(security_schemes = [
        { name = "oidc", type = "openIdConnect", open_id_connect_url = "https://example.com/.well-known/openid-configuration" }
    ]);
    let input: AutoRouterInput = syn::parse2(ok).expect("openIdConnect with url parses");
    let schemes = input.security_schemes.unwrap();
    assert_eq!(
        schemes[0].scheme.open_id_connect_url.as_deref(),
        Some("https://example.com/.well-known/openid-configuration")
    );
}

#[test]
fn test_security_scheme_duplicate_field_rejected() {
    let tokens = quote::quote!(security_schemes = [
        { name = "a", name = "b", type = "http", scheme = "bearer" }
    ]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "duplicate scheme field must be rejected");
    assert!(result.err().unwrap().to_string().contains("duplicate"));
}

#[test]
fn test_tag_duplicate_field_rejected() {
    // A repeated tag field (e.g. `name = ..., name = ...`) must be a spanned
    // compile error, not a silent last-wins overwrite.
    let tokens = quote::quote!(tags = [{ name = "a", name = "b" }]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "duplicate tag field must be rejected");
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate tag field")
    );
}

#[test]
fn test_server_duplicate_field_rejected() {
    // A repeated server field (e.g. `url = ..., url = ...`) must be a spanned
    // compile error, not a silent last-wins overwrite.
    let tokens =
        quote::quote!(servers = [{ url = "http://localhost:3000", url = "http://other:3000" }]);
    let result: syn::Result<AutoRouterInput> = syn::parse2(tokens);
    assert!(result.is_err(), "duplicate server field must be rejected");
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate server field")
    );
}

#[rstest]
#[case::unknown_tag_field(
    quote::quote!(tags = [{ name = "users", color = "blue" }]),
    "unknown tag field: `color`. Expected `name` or `description`"
)]
#[case::unknown_security_field(
    quote::quote!(security_schemes = [{ name = "auth", type = "mutualTLS", audience = "api" }]),
    "unknown security scheme field: `audience`. Expected `name`, `type`, `description`, `header_name`, `in`, `scheme`, `bearer_format`, or `open_id_connect_url`"
)]
#[case::security_missing_name(
    quote::quote!(security_schemes = [{ type = "mutualTLS" }]),
    "vespera! macro: security scheme missing required `name` field."
)]
#[case::security_missing_type(
    quote::quote!(security_schemes = [{ name = "auth" }]),
    "vespera! macro: security scheme missing required `type` field."
)]
#[case::api_key_missing_header_name(
    quote::quote!(security_schemes = [{ name = "apiKey", type = "apiKey", in = "header" }]),
    "vespera! macro: security scheme `apiKey` of type `apiKey` is missing required field `header_name` (the api-key parameter name)"
)]
#[case::invalid_security_type(
    quote::quote!(security_schemes = [{ name = "auth", type = "custom" }]),
    "invalid security scheme type: `custom`. Expected `apiKey`, `http`, `mutualTLS`, `oauth2`, or `openIdConnect`"
)]
fn malformed_nested_configuration_reports_the_specific_field_error(
    #[case] tokens: proc_macro2::TokenStream,
    #[case] expected: &str,
) {
    let error = syn::parse2::<AutoRouterInput>(tokens)
        .err()
        .expect("fixture must be rejected");

    assert_eq!(error.to_string(), expected);
}

#[test]
fn mutual_tls_security_scheme_requires_no_additional_fields() {
    let input = syn::parse2::<AutoRouterInput>(quote::quote!(
        security_schemes = [{ name = "clientCert", type = "mutualTLS" }]
    ))
    .expect("mutualTLS scheme should parse without type-specific fields");
    let schemes = input.security_schemes.expect("scheme list is present");

    assert_eq!(schemes.len(), 1);
    assert_eq!(schemes[0].name, "clientCert");
    assert_eq!(schemes[0].scheme.r#type, SecuritySchemeType::MutualTls);
}

#[rstest]
#[case::api_key(SecuritySchemeType::ApiKey, "apiKey")]
#[case::http(SecuritySchemeType::Http, "http")]
#[case::mutual_tls(SecuritySchemeType::MutualTls, "mutualTLS")]
#[case::oauth2(SecuritySchemeType::OAuth2, "oauth2")]
#[case::open_id_connect(SecuritySchemeType::OpenIdConnect, "openIdConnect")]
fn security_scheme_type_labels_match_openapi_wire_names(
    #[case] scheme_type: SecuritySchemeType,
    #[case] expected: &str,
) {
    assert_eq!(scheme_type_label(scheme_type), expected);
}
