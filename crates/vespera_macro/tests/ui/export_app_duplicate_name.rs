//! Two `export_app!`s of one crate may not share a name: their OpenAPI
//! sidecar, cache and export key are all named `<crate>/<App>`.
mod admin {
    vespera_macro::export_app!(App, dir = "missing_admin_routes");
}

mod public {
    vespera_macro::export_app!(App, dir = "missing_public_routes");
}

fn main() {}
