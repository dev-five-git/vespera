use axum::extract::Path;

#[vespera::route(path = "/{var}")]
pub async fn prefix_variable(Path(var): Path<String>) -> String {
    format!("prefix variable: {}", var)
}
