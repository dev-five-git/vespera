use vespera::axum::{Json, extract::Path};

use crate::models::schemas::{
    ApiResponse, Category, Inventory, Paginated, Product, Tag, Warehouse,
};

/// List products (paginated).
#[vespera::route(get, tags = ["catalog"])]
pub async fn list_products() -> Json<Paginated<Product>> {
    Json(Paginated::empty())
}

/// Get a product.
#[vespera::route(get, path = "/{id}", tags = ["catalog"])]
pub async fn get_product(Path(_id): Path<u64>) -> Json<ApiResponse<Product>> {
    Json(ApiResponse::ok(Product::default()))
}

/// Create a product.
#[vespera::route(post, tags = ["catalog"])]
pub async fn create_product(Json(body): Json<Product>) -> Json<ApiResponse<Product>> {
    Json(ApiResponse::ok(body))
}

/// Category tree (paginated, self-referential schema).
#[vespera::route(get, path = "/categories", tags = ["catalog"])]
pub async fn list_categories() -> Json<Paginated<Category>> {
    Json(Paginated::empty())
}

/// A product's tags.
#[vespera::route(get, path = "/{id}/tags", tags = ["catalog"])]
pub async fn product_tags(Path(_id): Path<u64>) -> Json<Vec<Tag>> {
    Json(Vec::new())
}

/// Inventory (paginated).
#[vespera::route(get, path = "/inventory", tags = ["catalog"])]
pub async fn list_inventory() -> Json<Paginated<Inventory>> {
    Json(Paginated::empty())
}

/// Warehouses.
#[vespera::route(get, path = "/warehouses", tags = ["catalog"])]
pub async fn list_warehouses() -> Json<Vec<Warehouse>> {
    Json(Vec::new())
}
