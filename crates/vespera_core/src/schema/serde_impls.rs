use super::{
    AdditionalProperties, Discriminator, ExternalDocumentation, Schema, SchemaRef, SchemaType,
    is_empty_properties, is_empty_required,
};
use serde::{
    Deserialize, Serialize,
    de::Error as DeError,
    ser::{SerializeSeq, SerializeStruct},
};
use std::collections::BTreeMap;

/// Serialize `Option<f64>` as integer when the value has no fractional part.
///
/// Ensures OpenAPI JSON uses `0` instead of `0.0` for integer constraints like
/// `minimum`/`maximum`, matching the convention that integer type bounds are integers.
#[cfg(test)]
#[allow(clippy::ref_option)] // serde serialize_with mandates &Option<T> signature
pub(super) fn serialize_number_constraint<S>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(v) if v.fract() == 0.0 => {
            // Float→int casts saturate in Rust, so an out-of-range
            // constraint (e.g. `1e20`) would silently become `i64::MAX`
            // and corrupt the generated spec.  Emit the integer form
            // only when it round-trips exactly back to the original
            // value; otherwise keep the `f64` rendering.
            #[allow(clippy::cast_possible_truncation)]
            let int_val = *v as i64;
            // Exact round-trip check is intentional: we emit the integer
            // form only when `i64 → f64` reproduces the original bits.
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            if int_val as f64 == *v {
                serializer.serialize_some(&int_val)
            } else {
                serializer.serialize_some(v)
            }
        }
        Some(v) => serializer.serialize_some(v),
        None => serializer.serialize_none(),
    }
}

struct NumberConstraint(f64);

impl Serialize for NumberConstraint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0.fract() == 0.0 {
            #[allow(clippy::cast_possible_truncation)]
            let int_val = self.0 as i64;
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            if int_val as f64 == self.0 {
                return int_val.serialize(serializer);
            }
        }
        self.0.serialize(serializer)
    }
}

struct NullableRefSchema<'a> {
    ref_path: &'a str,
}

impl Serialize for NullableRefSchema<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut out = serializer.serialize_struct("Schema", 1)?;
        out.serialize_field("$ref", self.ref_path)?;
        out.end()
    }
}

struct NullSchema;

impl Serialize for NullSchema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut out = serializer.serialize_struct("Schema", 1)?;
        out.serialize_field("type", &SchemaType::Null)?;
        out.end()
    }
}

struct NullableRefAnyOf<'a> {
    ref_path: &'a str,
}

impl Serialize for NullableRefAnyOf<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&NullableRefSchema {
            ref_path: self.ref_path,
        })?;
        seq.serialize_element(&NullSchema)?;
        seq.end()
    }
}

struct ExamplesWithLegacy<'a> {
    example: Option<&'a serde_json::Value>,
    examples: &'a [serde_json::Value],
}

impl Serialize for ExamplesWithLegacy<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let len = self.examples.len() + usize::from(self.example.is_some());
        let mut seq = serializer.serialize_seq(Some(len))?;
        if let Some(example) = self.example {
            seq.serialize_element(example)?;
        }
        for example in self.examples {
            seq.serialize_element(example)?;
        }
        seq.end()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub(super) enum SchemaTypeWire {
    Single(SchemaType),
    Multiple(Vec<SchemaType>),
}

impl SchemaTypeWire {
    pub(super) fn into_schema_type_and_nullable<E>(
        self,
    ) -> Result<(Option<SchemaType>, Option<bool>), E>
    where
        E: DeError,
    {
        match self {
            Self::Single(schema_type) => Ok((Some(schema_type), None)),
            Self::Multiple(schema_types) => {
                let nullable = schema_types.contains(&SchemaType::Null).then_some(true);
                let mut schema_type = None;
                for next_type in schema_types
                    .into_iter()
                    .filter(|schema_type| *schema_type != SchemaType::Null)
                {
                    if let Some(current_type) = schema_type
                        && current_type != next_type
                    {
                        return Err(E::custom(
                            "OpenAPI schema `type` arrays with multiple non-null types are not representable; use anyOf/oneOf instead",
                        ));
                    }
                    schema_type = Some(next_type);
                }
                // `["null"]` (or `["null","null"]`): a null-only `type` array.
                // Without this it would yield `(None, Some(true))` and
                // re-serialize to `{}` — silently dropping the null constraint.
                // Collapse to the equivalent singular `type:"null"` so the
                // schema round-trips losslessly.
                if schema_type.is_none() && nullable == Some(true) {
                    return Ok((Some(SchemaType::Null), None));
                }
                Ok((schema_type, nullable))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaDeserialize {
    #[serde(rename = "$ref")]
    ref_path: Option<String>,
    #[serde(rename = "type")]
    schema_type: Option<SchemaTypeWire>,
    format: Option<String>,
    title: Option<String>,
    description: Option<String>,
    default: Option<serde_json::Value>,
    example: Option<serde_json::Value>,
    examples: Option<Vec<serde_json::Value>>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    exclusive_minimum: Option<f64>,
    exclusive_maximum: Option<f64>,
    multiple_of: Option<f64>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    pattern: Option<String>,
    items: Option<SchemaRef>,
    prefix_items: Option<Vec<SchemaRef>>,
    min_items: Option<usize>,
    max_items: Option<usize>,
    unique_items: Option<bool>,
    properties: Option<BTreeMap<String, SchemaRef>>,
    required: Option<Vec<String>>,
    additional_properties: Option<AdditionalProperties>,
    min_properties: Option<usize>,
    max_properties: Option<usize>,
    r#enum: Option<Vec<serde_json::Value>>,
    all_of: Option<Vec<SchemaRef>>,
    any_of: Option<Vec<SchemaRef>>,
    one_of: Option<Vec<SchemaRef>>,
    not: Option<SchemaRef>,
    discriminator: Option<Discriminator>,
    nullable: Option<bool>,
    read_only: Option<bool>,
    write_only: Option<bool>,
    external_docs: Option<ExternalDocumentation>,
    #[serde(rename = "$defs")]
    defs: Option<BTreeMap<String, Schema>>,
    #[serde(rename = "$dynamicAnchor")]
    dynamic_anchor: Option<String>,
    #[serde(rename = "$dynamicRef")]
    dynamic_ref: Option<String>,
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SchemaDeserialize::deserialize(deserializer)?;
        let (schema_type, type_nullable) = wire.schema_type.map_or(Ok((None, None)), |wire| {
            wire.into_schema_type_and_nullable::<D::Error>()
        })?;
        let nullable = match type_nullable {
            Some(true) => Some(true),
            None => wire.nullable,
            Some(false) => wire.nullable.or(Some(false)),
        };
        Ok(Self {
            ref_path: wire.ref_path,
            schema_type,
            format: wire.format,
            title: wire.title,
            description: wire.description,
            default: wire.default,
            example: wire.example,
            examples: wire.examples,
            minimum: wire.minimum,
            maximum: wire.maximum,
            exclusive_minimum: wire.exclusive_minimum,
            exclusive_maximum: wire.exclusive_maximum,
            multiple_of: wire.multiple_of,
            min_length: wire.min_length,
            max_length: wire.max_length,
            pattern: wire.pattern,
            items: wire.items,
            prefix_items: wire.prefix_items,
            min_items: wire.min_items,
            max_items: wire.max_items,
            unique_items: wire.unique_items,
            properties: wire.properties,
            required: wire.required,
            additional_properties: wire.additional_properties,
            min_properties: wire.min_properties,
            max_properties: wire.max_properties,
            r#enum: wire.r#enum,
            all_of: wire.all_of,
            any_of: wire.any_of,
            one_of: wire.one_of,
            not: wire.not,
            discriminator: wire.discriminator,
            nullable,
            read_only: wire.read_only,
            write_only: wire.write_only,
            external_docs: wire.external_docs,
            defs: wire.defs,
            dynamic_anchor: wire.dynamic_anchor,
            dynamic_ref: wire.dynamic_ref,
        })
    }
}

/// Borrowing serializer for a nullable scalar `type` array (`[T, "null"]`).
///
/// Avoids the temporary two-element `Vec` the
/// `SchemaTypeWire::Multiple(vec![t, Null])` path allocated on **every**
/// nullable non-`$ref` schema during OpenAPI generation. Emits the identical
/// JSON array (`SchemaTypeWire` is `#[serde(untagged)]`, so `Multiple(vec)`
/// renders as a bare array), so the wire bytes are unchanged — mirrors the
/// existing zero-allocation [`NullableRefAnyOf`] serializer.
struct NullableScalarType(SchemaType);

impl Serialize for NullableScalarType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&self.0)?;
        seq.serialize_element(&SchemaType::Null)?;
        seq.end()
    }
}

/// Ordered field-group emitters for [`Schema`]'s hand-written [`Serialize`].
///
/// Each helper writes one contiguous group of the OpenAPI field sequence in the
/// exact order the single straight-line body used to. **Field order is part of
/// the emitted wire shape** (the generated `openapi.json` and every `insta`
/// snapshot are byte-compared), so the call sites in [`Schema::serialize`] must
/// stay in this order and no statement may migrate between groups.
impl Schema {
    /// `minimum` … `multipleOf`.
    fn serialize_numeric_constraints<S>(&self, out: &mut S) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if let Some(value) = self.minimum {
            out.serialize_field("minimum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.maximum {
            out.serialize_field("maximum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.exclusive_minimum {
            out.serialize_field("exclusiveMinimum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.exclusive_maximum {
            out.serialize_field("exclusiveMaximum", &NumberConstraint(value))?;
        }
        if let Some(value) = self.multiple_of {
            out.serialize_field("multipleOf", &NumberConstraint(value))?;
        }
        Ok(())
    }

    /// `minLength` … `pattern`.
    fn serialize_string_constraints<S>(&self, out: &mut S) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if let Some(value) = self.min_length {
            out.serialize_field("minLength", &value)?;
        }
        if let Some(value) = self.max_length {
            out.serialize_field("maxLength", &value)?;
        }
        if let Some(value) = &self.pattern {
            out.serialize_field("pattern", value)?;
        }
        Ok(())
    }

    /// `items` … `uniqueItems`.
    fn serialize_array_constraints<S>(&self, out: &mut S) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if let Some(value) = &self.items {
            out.serialize_field("items", value)?;
        }
        if let Some(value) = &self.prefix_items {
            out.serialize_field("prefixItems", value)?;
        }
        if let Some(value) = self.min_items {
            out.serialize_field("minItems", &value)?;
        }
        if let Some(value) = self.max_items {
            out.serialize_field("maxItems", &value)?;
        }
        if let Some(value) = self.unique_items {
            out.serialize_field("uniqueItems", &value)?;
        }
        Ok(())
    }

    /// `properties` … `maxProperties`.
    fn serialize_object_constraints<S>(&self, out: &mut S) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if !is_empty_properties(&self.properties) {
            out.serialize_field("properties", &self.properties)?;
        }
        if !is_empty_required(&self.required) {
            out.serialize_field("required", &self.required)?;
        }
        if let Some(value) = &self.additional_properties {
            out.serialize_field("additionalProperties", value)?;
        }
        if let Some(value) = self.min_properties {
            out.serialize_field("minProperties", &value)?;
        }
        if let Some(value) = self.max_properties {
            out.serialize_field("maxProperties", &value)?;
        }
        Ok(())
    }

    /// `enum` … `discriminator`.
    ///
    /// `nullable_ref` suppresses the explicit `anyOf` field: a nullable `$ref`
    /// already emitted its own `anyOf` in the head, and the validation at the
    /// top of [`Schema::serialize`] guarantees the two never both carry data.
    fn serialize_composition<S>(&self, out: &mut S, nullable_ref: bool) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if let Some(value) = &self.r#enum {
            out.serialize_field("enum", value)?;
        }
        if let Some(value) = &self.all_of {
            out.serialize_field("allOf", value)?;
        }
        if let Some(value) = &self.any_of
            && !nullable_ref
        {
            out.serialize_field("anyOf", value)?;
        }
        if let Some(value) = &self.one_of {
            out.serialize_field("oneOf", value)?;
        }
        if let Some(value) = &self.not {
            out.serialize_field("not", value)?;
        }
        if let Some(value) = &self.discriminator {
            out.serialize_field("discriminator", value)?;
        }
        Ok(())
    }

    /// `readOnly` … `$dynamicRef`.
    fn serialize_trailing_flags<S>(&self, out: &mut S) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if let Some(value) = self.read_only {
            out.serialize_field("readOnly", &value)?;
        }
        if let Some(value) = self.write_only {
            out.serialize_field("writeOnly", &value)?;
        }
        if let Some(value) = &self.external_docs {
            out.serialize_field("externalDocs", value)?;
        }
        if let Some(value) = &self.defs {
            out.serialize_field("$defs", value)?;
        }
        if let Some(value) = &self.dynamic_anchor {
            out.serialize_field("$dynamicAnchor", value)?;
        }
        if let Some(value) = &self.dynamic_ref {
            out.serialize_field("$dynamicRef", value)?;
        }
        Ok(())
    }
}

impl Serialize for Schema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let nullable_ref = self.nullable == Some(true) && self.ref_path.is_some();
        if nullable_ref && self.any_of.is_some() {
            return Err(serde::ser::Error::custom(
                "invalid Schema: nullable `$ref` serializes through anyOf and cannot also carry explicit any_of",
            ));
        }
        // A nullable `$ref` is emitted as `anyOf: [{$ref}, {type:null}]`; a
        // sibling `type` would then describe the SAME node twice and produce
        // ambiguous/invalid output (`anyOf` AND `type` at one level).  Vespera's
        // own `Schema::nullable_reference` always leaves `schema_type` None, so
        // this only fires for a hand-built `Schema` that mixed the two — reject
        // it like the `any_of` case above instead of serializing broken OpenAPI.
        if nullable_ref && self.schema_type.is_some() {
            return Err(serde::ser::Error::custom(
                "invalid Schema: nullable `$ref` serializes through anyOf and cannot also carry an explicit type; build it via Schema::nullable_reference",
            ));
        }
        let mut out = serializer.serialize_struct("Schema", 42)?;
        if let Some(ref_path) = &self.ref_path {
            if nullable_ref {
                out.serialize_field("anyOf", &NullableRefAnyOf { ref_path })?;
            } else {
                out.serialize_field("$ref", ref_path)?;
            }
        }
        if let Some(schema_type) = self.schema_type {
            // Nullable scalar → `[T, "null"]` via the borrowing
            // `NullableScalarType` (no temporary `Vec`); plain scalar → `T`
            // directly (`SchemaTypeWire::Single` is untagged, so a bare
            // `SchemaType` is byte-identical). Both avoid the previous
            // per-schema `SchemaTypeWire` value.
            if self.nullable == Some(true) {
                out.serialize_field("type", &NullableScalarType(schema_type))?;
            } else {
                out.serialize_field("type", &schema_type)?;
            }
        }
        if let Some(value) = &self.format {
            out.serialize_field("format", value)?;
        }
        if let Some(value) = &self.title {
            out.serialize_field("title", value)?;
        }
        if let Some(value) = &self.description {
            out.serialize_field("description", value)?;
        }
        if let Some(value) = &self.default {
            out.serialize_field("default", value)?;
        }
        match (&self.example, &self.examples) {
            (Some(example), Some(examples)) => {
                out.serialize_field(
                    "examples",
                    &ExamplesWithLegacy {
                        example: Some(example),
                        examples,
                    },
                )?;
            }
            (Some(example), None) => {
                out.serialize_field(
                    "examples",
                    &ExamplesWithLegacy {
                        example: Some(example),
                        examples: &[],
                    },
                )?;
            }
            (None, Some(examples)) => {
                out.serialize_field("examples", examples)?;
            }
            (None, None) => {}
        }
        // Order is load-bearing: these six calls reproduce the exact field
        // sequence of the previous straight-line body.
        self.serialize_numeric_constraints(&mut out)?;
        self.serialize_string_constraints(&mut out)?;
        self.serialize_array_constraints(&mut out)?;
        self.serialize_object_constraints(&mut out)?;
        self.serialize_composition(&mut out, nullable_ref)?;
        self.serialize_trailing_flags(&mut out)?;
        out.end()
    }
}
