#[vespera::route(get)]
pub async fn health() -> &'static str {
    "ok"
}

/// A route a `macro_rules!` macro generates: this file never spells its
/// declaration out, yet `vespera!` mounts it.
macro_rules! probe_route {
    ($name:ident, $path:literal, $body:literal) => {
        #[vespera::route(get, path = $path)]
        pub async fn $name() -> &'static str {
            $body
        }
    };
}

probe_route!(liveness, "/live", "alive");
