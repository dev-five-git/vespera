//! `vespera!` security scheme / server DSL errors: an unknown scheme type, an
//! `apiKey` scheme without `in`, and a server URL without an http(s) scheme.
use vespera_macro::vespera;

fn unknown_scheme_type() {
    let _ = vespera!(security_schemes = [{ name = "auth", type = "magic" }]);
}

fn api_key_missing_in() {
    let _ = vespera!(security_schemes = [{ name = "key", type = "apiKey", header_name = "X-Key" }]);
}

fn server_url_without_scheme() {
    let _ = vespera!(servers = [{ url = "localhost:3000" }]);
}

fn main() {}
