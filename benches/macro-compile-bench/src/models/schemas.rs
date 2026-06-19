//! All benchmark schemas, deliberately cross-referenced so the OpenAPI
//! generator resolves the same DTO many times (the per-reference cost the
//! compile-time benchmark is meant to surface). `Default` is derived so route
//! handlers stay one-liners — only the type *signatures* drive expansion cost.

use serde::{Deserialize, Serialize};
use vespera::Schema;

// ── Users domain ─────────────────────────────────────────────────────────

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    pub street: String,
    pub city: String,
    pub country: String,
    pub postal_code: String,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Permission {
    pub id: u32,
    pub action: String,
    pub resource: String,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub id: u32,
    pub name: String,
    pub permissions: Vec<Permission>,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub display_name: String,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub address: Address,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: u64,
    pub username: String,
    pub email: String,
    pub profile: Profile,
    pub roles: Vec<Role>,
    pub created_at: String,
}

// ── Catalog domain ───────────────────────────────────────────────────────

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub slug: String,
    /// Self-referential: exercises circular-schema handling.
    pub parent: Option<Box<Category>>,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub price: f64,
    pub category: Category,
    pub tags: Vec<Tag>,
    pub in_stock: bool,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Warehouse {
    pub id: u32,
    pub name: String,
    pub location: Address,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub product: Product,
    pub warehouse: Warehouse,
    pub quantity: u32,
    pub reserved: u32,
}

// ── Orders domain ────────────────────────────────────────────────────────

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    #[default]
    Pending,
    Paid,
    Shipped,
    Delivered,
    Cancelled,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct OrderItem {
    pub product: Product,
    pub quantity: u32,
    pub unit_price: f64,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    pub id: u64,
    pub customer: User,
    pub items: Vec<OrderItem>,
    pub status: OrderStatus,
    pub total: f64,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Payment {
    pub id: u64,
    pub order_id: u64,
    pub method: String,
    pub amount: f64,
    pub paid: bool,
}

// ── Generic envelopes (Wrapper<T> — exercises the generic schema path) ─────

#[derive(Clone, Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Paginated<T: Serialize> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Clone, Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: u32,
    pub message: String,
}

impl<T: Serialize> Paginated<T> {
    /// One empty page — keeps handlers free of `T` construction.
    pub fn empty() -> Self {
        Self { items: Vec::new(), total: 0, page: 1, per_page: 20 }
    }
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self { data, success: true, message: None }
    }
}
