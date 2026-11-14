pub mod prefix;

#[vespera::route(get, path = "/multi-path/{var1}")]
pub async fn mod_file_with_multi_path_single(
    vespera::axum::extract::Path(var1): vespera::axum::extract::Path<String>,
) -> &'static str {
    println!("var1: {}", var1);
    "multi path"
}

// multi path
#[vespera::route(get, path = "/multi-path/{arg}/{var1}/{var2}")]
pub async fn mod_file_with_multi_path(
    vespera::axum::extract::Path((arg2, var1, var2)): vespera::axum::extract::Path<(
        String,
        String,
        String,
    )>,
) -> &'static str {
    println!("arg: {}", arg2);
    println!("var1: {}", var1);
    println!("var2: {}", var2);
    "multi path"
}

// multi path
#[vespera::route(get, path = "/multi-path2/{arg}/{var1}/{var2}")]
pub async fn mod_file_with_multi_path_2(
    vespera::axum::extract::Path(path): vespera::axum::extract::Path<(String, String, String)>,
) -> &'static str {
    println!("arg: {:?}", path);
    "multi path"
}

// axum 0.7 `:param` / `*rest` notation: normalized at macro time to axum 0.8's
// `{id}` / `{*rest}` (router and OpenAPI alike).
#[vespera::route(get, path = "/legacy/:id/files/*rest")]
pub async fn legacy_path_notation(
    vespera::axum::extract::Path((id, rest)): vespera::axum::extract::Path<(u32, String)>,
) -> String {
    format!("{id}:{rest}")
}

// A parameter after a static prefix within its segment (axum's router does
// not support text AFTER a parameter: `/{name}.png` is a compile error).
#[vespera::route(get, path = "/report-{year}")]
pub async fn prefixed_segment_param(
    vespera::axum::extract::Path(year): vespera::axum::extract::Path<u16>,
) -> String {
    format!("report {year}")
}
