use sea_orm::prelude::Decimal;
use serde::{Deserialize, Serialize};
use vespera::Schema;
use vespera::axum::Json;

#[derive(Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub tax_rate: Decimal,
    pub discount_rate: Option<Decimal>,
    pub min_price: Decimal,
    pub max_price: Decimal,
    pub max_items: usize,
    pub retry_count: u8,
    pub priority: i32,
    pub separator: char,
    pub delimiter: Option<char>,
}

#[derive(Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfigRequest {
    pub tax_rate: Option<Decimal>,
    pub discount_rate: Option<Decimal>,
    pub min_price: Option<Decimal>,
    pub max_price: Option<Decimal>,
    pub max_items: Option<usize>,
    pub retry_count: Option<u8>,
    pub priority: Option<i32>,
    pub separator: Option<char>,
    pub delimiter: Option<char>,
}

/// Get current config
#[vespera::route(get, tags = ["config"])]
pub async fn get_config() -> Json<Config> {
    Json(Config {
        tax_rate: Decimal::new(10, 2),
        discount_rate: Some(Decimal::new(5, 2)),
        min_price: Decimal::new(100, 0),
        max_price: Decimal::new(10000, 0),
        max_items: 100,
        retry_count: 3,
        priority: 0,
        separator: ',',
        delimiter: Some('|'),
    })
}

/// Update config
#[vespera::route(patch, tags = ["config"])]
pub async fn update_config(Json(req): Json<UpdateConfigRequest>) -> Json<Config> {
    let _ = crate::models::config::Model {
        id: 1,
        temperature: Decimal::new(10, 2),
    };
    let current = Config {
        tax_rate: Decimal::new(10, 2),
        discount_rate: Some(Decimal::new(5, 2)),
        min_price: Decimal::new(100, 0),
        max_price: Decimal::new(10000, 0),
        max_items: 100,
        retry_count: 3,
        priority: 0,
        separator: ',',
        delimiter: Some('|'),
    };

    Json(Config {
        tax_rate: req.tax_rate.unwrap_or(current.tax_rate),
        discount_rate: req.discount_rate.or(current.discount_rate),
        min_price: req.min_price.unwrap_or(current.min_price),
        max_price: req.max_price.unwrap_or(current.max_price),
        max_items: req.max_items.unwrap_or(current.max_items),
        retry_count: req.retry_count.unwrap_or(current.retry_count),
        priority: req.priority.unwrap_or(current.priority),
        separator: req.separator.unwrap_or(current.separator),
        delimiter: req.delimiter.or(current.delimiter),
    })
}
