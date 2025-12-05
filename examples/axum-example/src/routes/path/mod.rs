pub mod prefix;

#[vespera::route(get, path = "/multi-path/{var1}")]
pub async fn mod_file_with_test_struct(
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
