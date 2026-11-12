//! `vespera!` argument errors: a misspelled key, a repeated key, and doc-route
//! URLs axum would panic on at router build time.
use vespera_macro::vespera;

fn unknown_field() {
    let _ = vespera!(titel = "My API");
}

fn duplicate_field() {
    let _ = vespera!(title = "A", title = "B");
}

fn docs_url_without_slash() {
    let _ = vespera!(docs_url = "docs");
}

fn docs_urls_colliding() {
    let _ = vespera!(docs_url = "/docs", redoc_url = "/docs");
}

fn main() {}
