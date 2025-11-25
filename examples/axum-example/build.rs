use vespera::vespera_openapi;
fn main() {
    // Generate OpenAPI JSON using vespera
    let json = vespera_openapi!();
    std::fs::write("openapi.json", json).unwrap();
}
