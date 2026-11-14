//! `export_app!` route folders must be module folders under `src/`.
vespera_macro::export_app!(AbsoluteApp, dir = "/srv/routes");

vespera_macro::export_app!(DashedApp, dir = "my-routes");

fn main() {}
