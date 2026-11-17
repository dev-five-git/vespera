//! Compile-fail: a field whose `#[serde(with = "..")]` codec is neither a
//! well-known module (chrono, time, humantime, serde_with, serde_bytes) nor a
//! string type has an unknown wire shape, so `#[derive(Schema)]` asks for
//! `#[schema(value_type = ..)]` on the serde attribute.

#[derive(vespera::Schema)]
pub struct Event {
    pub id: u64,
    #[serde(with = "hex_bytes")]
    pub checksum: Vec<u8>,
}

fn main() {}
