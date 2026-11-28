//! The default value a `#[serde(default)]` field of a type gets without an
//! explicit `default = "fn"` ([`get_type_default`]): the zero of a primitive or
//! ecosystem type, decided by how the type path resolves in its module.

use syn::Type;

use super::{first_generic_type_arg, last_path_segment};

/// Names whose default below is the value of an ecosystem type (SeaORM /
/// chrono / rust_decimal / uuid), which a crate may also define itself.
const ECOSYSTEM_DEFAULT_NAMES: &[&str] = &[
    "Decimal",
    "Uuid",
    "Date",
    "Time",
    "DateTime",
    "DateTimeUtc",
    "DateTimeWithTimeZone",
    "NaiveDate",
    "NaiveTime",
    "NaiveDateTime",
];

/// Whether the path `segments`, written in the module `source_module` (the
/// module of the struct the field is declared in), names an ecosystem type.
/// A crate-local type of the same name (`struct Decimal(i64)`) advertised
/// `"0"` / `"1970-01-01"` before. Without a known module (`source_module` is
/// empty) the name is taken as the ecosystem type, as before.
fn names_ecosystem_type(segments: &[String], source_module: &[String]) -> bool {
    use crate::schema_macro::module_scope::{Resolved, resolve_name};
    let mut path = match resolve_name(segments, source_module) {
        Resolved::Path(path) => path,
        Resolved::Unknown => return true,
    };
    // A crate-internal re-export (`pub use rust_decimal::Decimal;` in
    // `crate::types`, reached through `use crate::types::Decimal;`) names the
    // external root one module further in: follow the chain (R34-SM4-L2).
    for _ in 0..8 {
        let Some((name, module)) = path
            .split_last()
            .filter(|(_, module)| module.first().map(String::as_str) == Some("crate"))
        else {
            break;
        };
        match resolve_name(std::slice::from_ref(name), module) {
            Resolved::Path(next) if next != path => path = next,
            _ => break,
        }
    }
    // `vespera` re-exports chrono (`vespera::chrono`, R42-SM4-L1); its other
    // items are not ecosystem types.
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["sea_orm" | "chrono" | "rust_decimal" | "uuid" | "time", ..] | ["vespera", "chrono", ..]
    )
}

/// Get type-specific default value for simple #[serde(default)].
///
/// `source_module` is the absolute module (`crate::..`) the field's type is
/// written in; it resolves an ecosystem-looking name (`Decimal`, `Date`).
/// Empty when unknown.
pub fn get_type_default(ty: &Type, source_module: &[String]) -> Option<serde_json::Value> {
    match ty {
        Type::Path(type_path) => type_path.path.segments.last().and_then(|segment| {
            let name = segment.ident.to_string();
            if ECOSYSTEM_DEFAULT_NAMES.contains(&name.as_str()) {
                let segments: Vec<String> = type_path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect();
                if !names_ecosystem_type(&segments, source_module) {
                    return None;
                }
            }
            match name.as_str() {
                "String" => Some(serde_json::Value::String(String::new())),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Some(serde_json::Value::Number(serde_json::Number::from(0))),
                "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => {
                    Some(serde_json::Value::Array(Vec::new()))
                }
                "HashMap" | "BTreeMap" => Some(serde_json::Value::Object(serde_json::Map::new())),
                // `rust_decimal` serializes as a string.
                "Decimal" => Some(serde_json::Value::String("0".to_string())),
                "f32" | "f64" => Some(serde_json::Value::Number(
                    serde_json::Number::from_f64(0.0)
                        .unwrap_or_else(|| serde_json::Number::from(0)),
                )),
                "bool" => Some(serde_json::Value::Bool(false)),
                "Option" => Some(serde_json::Value::Null),
                "Uuid" => Some(serde_json::Value::String(
                    "00000000-0000-0000-0000-000000000000".to_string(),
                )),
                // chrono's epoch as each zone serializes it (`Local` depends
                // on the machine: no documented value).
                "DateTimeUtc" => Some(serde_json::Value::String(
                    "1970-01-01T00:00:00Z".to_string(),
                )),
                "DateTimeWithTimeZone" => Some(serde_json::Value::String(
                    "1970-01-01T00:00:00+00:00".to_string(),
                )),
                "DateTime" => match first_generic_type_arg(segment)
                    .and_then(last_path_segment)
                    .map(|zone| zone.ident.to_string())
                    .as_deref()
                {
                    Some("Utc") => Some(serde_json::Value::String(
                        "1970-01-01T00:00:00Z".to_string(),
                    )),
                    Some("FixedOffset") => Some(serde_json::Value::String(
                        "1970-01-01T00:00:00+00:00".to_string(),
                    )),
                    // The bare name is SeaORM's `NaiveDateTime` alias.
                    None if segment.arguments.is_none() => {
                        Some(serde_json::Value::String("1970-01-01T00:00:00".to_string()))
                    }
                    _ => None,
                },
                "NaiveDateTime" => {
                    Some(serde_json::Value::String("1970-01-01T00:00:00".to_string()))
                }
                // `Date` is SeaORM's `NaiveDate` alias.
                "NaiveDate" | "Date" => Some(serde_json::Value::String("1970-01-01".to_string())),
                "NaiveTime" | "Time" => Some(serde_json::Value::String("00:00:00".to_string())),
                _ => None,
            }
        }),
        _ => None,
    }
}
