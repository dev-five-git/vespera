//! `serde_json::Map` leaves of the Schema assertions (sidecar of
//! `schema_assertions`).

use syn::Type;

/// `serde_json::Map` (any path through a `serde_json` segment, `vespera`
/// re-export and the `serde_json::map::Map` module path included, and the bare
/// `Map` of `use serde_json::Map;`): documented as a JSON object of any members
/// by the type-schema conversion, so it is exempt like the `Value` leaf. Must
/// agree with the conversion's `is_serde_json_map_path`.
pub(super) fn is_serde_json_map_leaf(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    let segments = &type_path.path.segments;
    segments.last().is_some_and(|last| last.ident == "Map")
        && (segments.len() == 1
            || segments
                .iter()
                .rev()
                .skip(1)
                .any(|segment| segment.ident == "serde_json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ty(src: &str) -> Type {
        syn::parse_str::<Type>(src).expect("valid type")
    }

    #[test]
    fn serde_json_map_is_exempt_through_any_serde_json_path() {
        assert!(is_serde_json_map_leaf(&parse_ty(
            "serde_json::Map<String, u8>"
        )));
        assert!(is_serde_json_map_leaf(&parse_ty(
            "vespera::serde_json::Map<String, serde_json::Value>"
        )));
        assert!(is_serde_json_map_leaf(&parse_ty("::serde_json::Map")));
        assert!(is_serde_json_map_leaf(&parse_ty(
            "serde_json::map::Map<String, u8>"
        )));
        assert!(!is_serde_json_map_leaf(&parse_ty("foo::Map")));
        assert!(is_serde_json_map_leaf(&parse_ty("Map<String, u8>")));
        assert!(!is_serde_json_map_leaf(&parse_ty("&serde_json::Map")));
    }
}
