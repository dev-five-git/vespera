use vespera::axum::{Json, extract::Path};

use crate::models::schemas::{
    ApiResponse, Order, OrderItem, OrderStatus, Paginated, Payment, User,
};

/// List orders (paginated).
#[vespera::route(get, tags = ["orders"])]
pub async fn list_orders() -> Json<Paginated<Order>> {
    Json(Paginated::empty())
}

/// Get an order.
#[vespera::route(get, path = "/{id}", tags = ["orders"])]
pub async fn get_order(Path(_id): Path<u64>) -> Json<ApiResponse<Order>> {
    Json(ApiResponse::ok(Order::default()))
}

/// Create an order.
#[vespera::route(post, tags = ["orders"])]
pub async fn create_order(Json(body): Json<Order>) -> Json<ApiResponse<Order>> {
    Json(ApiResponse::ok(body))
}

/// An order's items.
#[vespera::route(get, path = "/{id}/items", tags = ["orders"])]
pub async fn order_items(Path(_id): Path<u64>) -> Json<Vec<OrderItem>> {
    Json(Vec::new())
}

/// An order's customer.
#[vespera::route(get, path = "/{id}/customer", tags = ["orders"])]
pub async fn order_customer(Path(_id): Path<u64>) -> Json<ApiResponse<User>> {
    Json(ApiResponse::ok(User::default()))
}

/// An order's payments.
#[vespera::route(get, path = "/{id}/payments", tags = ["orders"])]
pub async fn order_payments(Path(_id): Path<u64>) -> Json<Vec<Payment>> {
    Json(Vec::new())
}

/// Update an order's status.
#[vespera::route(patch, path = "/{id}/status", tags = ["orders"])]
pub async fn update_status(
    Path(_id): Path<u64>,
    Json(_status): Json<OrderStatus>,
) -> Json<ApiResponse<Order>> {
    Json(ApiResponse::ok(Order::default()))
}
