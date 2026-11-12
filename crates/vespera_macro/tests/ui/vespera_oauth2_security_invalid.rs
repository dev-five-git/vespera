//! OAuth2 / scoped-security DSL errors: an `oauth2` scheme without `flows`, a
//! flow missing a URL OpenAPI requires, a top-level requirement asking for a
//! scope no flow declares, and a malformed `#[route(security = [...])]` entry.
use vespera_macro::{route, vespera};

fn oauth2_without_flows() {
    let _ = vespera!(security_schemes = [{ name = "oauth2", type = "oauth2" }]);
}

fn flow_missing_token_url() {
    let _ = vespera!(security_schemes = [{
        name = "oauth2",
        type = "oauth2",
        flows = { authorization_code = { authorization_url = "https://auth.example.com/authorize" } },
    }]);
}

fn undeclared_scope() {
    let _ = vespera!(
        security_schemes = [{
            name = "oauth2",
            type = "oauth2",
            flows = {
                client_credentials = {
                    token_url = "https://auth.example.com/token",
                    scopes = { "read:users" = "Read users" },
                },
            },
        }],
        security = [("oauth2", ["read:users", "admin"])],
    );
}

#[route(get, security = [("oauth2", "read:users")])]
pub async fn scopes_not_an_array() {}

fn main() {}
