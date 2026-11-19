//! `serde_json::Map` leaves of the Schema assertions (sidecar of
//! `schema_assertions`).

use syn::Type;

/// `serde_json::Map` (any path through a `serde_json` segment, `vespera`
/// re-export included): documented as a JSON object of any members by the
/// type-schema conversion, so it is exempt like the `Value` leaf.
pub(super) fn is_serde_json_map_leaf(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    let segments = &type_path.path.segments;
    segments.last().is_some_and(|last| last.ident == "Map")
        && segments
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|parent| parent.ident == "serde_json")
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
        assert!(!is_serde_json_map_leaf(&parse_ty("foo::Map")));
        assert!(!is_serde_json_map_leaf(&parse_ty("Map")));
        assert!(!is_serde_json_map_leaf(&parse_ty("&serde_json::Map")));
    }
}
