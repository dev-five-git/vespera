//! A `schema_type!` source whose serde function paths are relative to THIS
//! module; DTOs derived from it elsewhere (`routes::memos`) resolve them here.
use serde::{Deserialize, Serialize};

pub fn default_theme() -> String {
    "light".to_string()
}

pub mod codec {
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Clone, vespera::Schema)]
pub struct Profile {
    #[serde(default = "default_theme")]
    #[schema(default = "light")]
    pub theme: String,
    #[serde(with = "codec")]
    #[schema(value_type = String)]
    pub score: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bio: String,
}
