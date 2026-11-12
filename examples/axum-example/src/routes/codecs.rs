//! serde attributes that change the wire shape: field-level `with` /
//! `serialize_with` / `deserialize_with` and container-level `into` /
//! `from` / `try_from`, documented with `#[schema(value_type = ..)]` — plus
//! SeaORM's `Json` / `JsonValue` aliases of `serde_json::Value`.

use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::Json,
    chrono::{DateTime, TimeZone, Utc},
    schema_type,
};

/// Timestamps written by custom serde functions.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct EventTimes {
    /// Unix seconds (`chrono::serde::ts_seconds`) — an integer, not a
    /// date-time string.
    #[serde(with = "vespera::chrono::serde::ts_seconds")]
    #[schema(value_type = i64)]
    pub created_at: DateTime<Utc>,
    /// Optional unix milliseconds.
    #[serde(with = "vespera::chrono::serde::ts_milliseconds_option")]
    #[schema(value_type = Option<i64>)]
    pub expires_at: Option<DateTime<Utc>>,
    // A custom codec without `value_type`: its shape is unknowable, so it is
    // documented as any value with a note (never as `array<uint8>`).
    #[serde(with = "hex_bytes")]
    pub checksum: Vec<u8>,
}

/// Lower-case hex string codec for byte buffers.
mod hex_bytes {
    use std::fmt::Write;

    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(hex, "{byte:02x}");
        }
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let hex = String::deserialize(deserializer)?;
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or_else(|| D::Error::custom("invalid hex"))
            })
            .collect()
    }
}

/// An RGB colour that travels as `"#rrggbb"` in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(into = "String", try_from = "String")]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<Rgb> for String {
    fn from(c: Rgb) -> Self {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    }
}

impl TryFrom<String> for Rgb {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let channel = |i: usize| {
            value
                .get(i..i + 2)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        };
        match (value.len(), value.starts_with('#')) {
            (7, true) => match (channel(1), channel(3), channel(5)) {
                (Some(r), Some(g), Some(b)) => Ok(Self { r, g, b }),
                _ => Err(format!("invalid colour {value:?}")),
            },
            _ => Err(format!("invalid colour {value:?}")),
        }
    }
}

/// The serialized shape of [`Money`].
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct MoneyWire {
    /// Decimal amount as a string, e.g. `"12.34"`.
    pub amount: String,
    /// ISO 4217 code.
    pub currency: String,
}

/// An amount: clients SEND `{cents, currency}` (the `MoneyInput` request
/// component) but RECEIVE the `into` type's shape (`MoneyWire`).
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(into = "MoneyWire")]
pub struct Money {
    pub cents: i64,
    pub currency: String,
}

impl From<Money> for MoneyWire {
    fn from(money: Money) -> Self {
        Self {
            amount: format!("{}.{:02}", money.cents / 100, money.cents % 100),
            currency: money.currency,
        }
    }
}

/// A Unix-seconds timestamp whose container-level `value_type` documents it
/// for both directions.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(transparent)]
#[schema(value_type = i64)]
pub struct UnixSeconds(#[serde(with = "vespera::chrono::serde::ts_seconds")] pub DateTime<Utc>);

/// A free-form JSON payload in every spelling vespera maps to `{}`.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct JsonPayloads {
    /// SeaORM's `Json` (= `serde_json::Value`): any JSON value.
    pub body: sea_orm::entity::prelude::Json,
    /// SeaORM's `JsonValue` alias.
    pub meta: Option<sea_orm::JsonValue>,
    /// `serde_json::Value` itself.
    pub extra: serde_json::Value,
}

schema_type!(DocumentResponse from crate::models::document::Model, name = "DocumentResponse");

fn sample_times() -> EventTimes {
    EventTimes {
        created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        expires_at: Some(Utc.timestamp_opt(1_800_000_000, 0).unwrap()),
        checksum: vec![0xde, 0xad, 0xbe, 0xef],
    }
}

/// Timestamps as unix seconds / milliseconds.
#[vespera::route(get, path = "/times", tags = ["codecs"])]
pub async fn get_times() -> Json<EventTimes> {
    Json(sample_times())
}

/// Echo timestamps back.
#[vespera::route(post, path = "/times", tags = ["codecs"])]
pub async fn echo_times(Json(times): Json<EventTimes>) -> Json<EventTimes> {
    Json(times)
}

/// Echo a colour (`"#rrggbb"` both ways).
#[vespera::route(post, path = "/color", tags = ["codecs"])]
pub async fn echo_color(Json(color): Json<Rgb>) -> Json<Rgb> {
    Json(color)
}

/// Accept `{cents, currency}`, answer `{amount, currency}`.
#[vespera::route(post, path = "/money", tags = ["codecs"])]
pub async fn echo_money(Json(money): Json<Money>) -> Json<Money> {
    Json(money)
}

/// Echo a unix-seconds timestamp.
#[vespera::route(post, path = "/stamp", tags = ["codecs"])]
pub async fn echo_stamp(Json(stamp): Json<UnixSeconds>) -> Json<UnixSeconds> {
    Json(stamp)
}

/// Echo free-form JSON payloads.
#[vespera::route(post, path = "/json", tags = ["codecs"])]
pub async fn echo_json(Json(payloads): Json<JsonPayloads>) -> Json<JsonPayloads> {
    Json(payloads)
}

/// A SeaORM model with `Json` / `Option<JsonValue>` columns.
#[vespera::route(get, path = "/document", tags = ["codecs"])]
pub async fn get_document() -> Json<DocumentResponse> {
    let model = crate::models::document::Model {
        id: 1,
        body: serde_json::json!([1, "two", {"three": 3}]),
        meta: None,
    };
    Json(DocumentResponse::from(model))
}
