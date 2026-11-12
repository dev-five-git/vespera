//! serde attributes that replace a type's own `Serialize` / `Deserialize`
//! and therefore change its wire shape:
//!
//! - field level: `with = "module"`, `serialize_with = "fn"`,
//!   `deserialize_with = "fn"` — the field is written / read by user code
//!   whose output shape vespera cannot see;
//! - container level: `into = "T"` (serialized as `T`), `from = "T"` /
//!   `try_from = "T"` (deserialized from `T`).

use super::{SerdeSide, extract::walk_serde_keys};

/// Which directions of a field go through a custom serde function.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FieldCodec {
    /// `with` or `serialize_with`.
    pub serialize: bool,
    /// `with` or `deserialize_with`.
    pub deserialize: bool,
}

impl FieldCodec {
    /// The field is converted by a custom function on `side`.
    #[must_use]
    pub const fn custom_for(self, side: SerdeSide) -> bool {
        match side {
            SerdeSide::Serialize => self.serialize,
            SerdeSide::Deserialize => self.deserialize,
        }
    }
}

/// Collect the field-level `with` / `serialize_with` / `deserialize_with` keys.
pub fn extract_field_codec(attrs: &[syn::Attribute]) -> FieldCodec {
    let mut codec = FieldCodec::default();
    walk_serde_keys(attrs, |meta| {
        let both = meta.path.is_ident("with");
        codec.serialize |= both || meta.path.is_ident("serialize_with");
        codec.deserialize |= both || meta.path.is_ident("deserialize_with");
        Ok(false)
    });
    codec
}

/// The type a container is converted through on `side`:
/// `#[serde(into = "T")]` when serializing, `#[serde(from = "T")]` /
/// `#[serde(try_from = "T")]` when deserializing.
pub fn extract_container_conversion(
    attrs: &[syn::Attribute],
    side: SerdeSide,
) -> Option<syn::Type> {
    let mut found = None;
    walk_serde_keys(attrs, |meta| {
        let matches = match side {
            SerdeSide::Serialize => meta.path.is_ident("into"),
            SerdeSide::Deserialize => meta.path.is_ident("from") || meta.path.is_ident("try_from"),
        };
        if !matches {
            return Ok(false);
        }
        let ty = meta.value()?.parse::<syn::LitStr>()?.parse::<syn::Type>()?;
        found.get_or_insert(ty);
        Ok(true)
    });
    found
}

#[cfg(test)]
mod tests {
    use quote::ToTokens;
    use rstest::rstest;

    use super::*;

    fn attrs(src: &str) -> Vec<syn::Attribute> {
        syn::parse_str::<syn::ItemStruct>(&format!("{src} struct S;"))
            .unwrap()
            .attrs
    }

    #[rstest]
    #[case::none("#[serde(rename = \"x\")]", false, false)]
    #[case::with("#[serde(with = \"m\")]", true, true)]
    #[case::serialize_with("#[serde(serialize_with = \"f\")]", true, false)]
    #[case::deserialize_with("#[serde(deserialize_with = \"f\")]", false, true)]
    #[case::split(
        "#[serde(serialize_with = \"f\")] #[serde(default, deserialize_with = \"g\")]",
        true,
        true
    )]
    #[case::other_attr("#[schema(with = \"m\")]", false, false)]
    fn field_codec(#[case] src: &str, #[case] serialize: bool, #[case] deserialize: bool) {
        let codec = extract_field_codec(&attrs(src));
        assert_eq!(
            codec,
            FieldCodec {
                serialize,
                deserialize
            }
        );
        assert_eq!(codec.custom_for(SerdeSide::Serialize), serialize);
        assert_eq!(codec.custom_for(SerdeSide::Deserialize), deserialize);
    }

    #[rstest]
    #[case::into_serialize("#[serde(into = \"Wire\")]", SerdeSide::Serialize, Some("Wire"))]
    #[case::into_not_deserialize("#[serde(into = \"Wire\")]", SerdeSide::Deserialize, None)]
    #[case::from(
        "#[serde(from = \"Vec<u8>\")]",
        SerdeSide::Deserialize,
        Some("Vec < u8 >")
    )]
    #[case::try_from(
        "#[serde(try_from = \"String\")]",
        SerdeSide::Deserialize,
        Some("String")
    )]
    #[case::from_not_serialize("#[serde(from = \"String\")]", SerdeSide::Serialize, None)]
    #[case::first_wins(
        "#[serde(into = \"A\")] #[serde(into = \"B\")]",
        SerdeSide::Serialize,
        Some("A")
    )]
    #[case::unparseable_type("#[serde(into = \"1 +\")]", SerdeSide::Serialize, None)]
    #[case::unrelated("#[serde(rename_all = \"camelCase\")]", SerdeSide::Serialize, None)]
    fn container_conversion(
        #[case] src: &str,
        #[case] side: SerdeSide,
        #[case] expected: Option<&str>,
    ) {
        let found = extract_container_conversion(&attrs(src), side)
            .map(|ty| ty.to_token_stream().to_string());
        assert_eq!(found.as_deref(), expected);
    }
}
