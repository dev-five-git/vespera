use super::{Schema, SchemaRef, SchemaType, is_empty_properties, is_empty_required};
use serde::{
    Deserialize, Serialize,
    de::Error as DeError,
    ser::{SerializeSeq, SerializeStruct},
};

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
        Some(v) => serializer.serialize_some(&NumberConstraint(*v)),
        None => serializer.serialize_none(),
    }
}

/// The `i64` rendering of `value`, or `None` when `value` is not a whole number
/// that survives the `f64 → i64 → f64` round trip.
///
/// Float→int casts SATURATE in Rust, so an out-of-range constraint (`1e20`,
/// or exactly `2^63`, whose saturated `i64::MAX` rounds back to `2^63` as an
/// `f64`) would silently become `i64::MAX` and corrupt the generated spec.
/// Only whole numbers in `[-2^63, 2^63)` take the integer form; every such
/// `f64` converts to `i64` exactly.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn lossless_integer_form(value: f64) -> Option<i64> {
    /// `2^63` — the first whole `f64` beyond `i64::MAX`.
    const I64_BOUND: f64 = 9_223_372_036_854_775_808.0;
    if value.fract() != 0.0 || !(-I64_BOUND..I64_BOUND).contains(&value) {
        return None;
    }
    Some(value as i64)
}

/// A numeric constraint (`minimum`, `multipleOf`, ...): whole numbers take the
/// integer form, and a non-finite value is an error — `serde_json` would write
/// NaN / ±inf as `null`, which is not a valid JSON Schema number.
struct NumberConstraint(f64);

impl Serialize for NumberConstraint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if !self.0.is_finite() {
            return Err(serde::ser::Error::custom(format_args!(
                "invalid Schema: numeric constraint must be finite, got {}",
                self.0
            )));
        }
        match lossless_integer_form(self.0) {
            Some(int_val) => int_val.serialize(serializer),
            None => self.0.serialize(serializer),
        }
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

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum SchemaTypeWire {
    Single(SchemaType),
    Multiple(Vec<SchemaType>),
}

/// The schema type named by `value`, or `None` for an unknown name.
fn parse_type_name(value: &str) -> Option<SchemaType> {
    SchemaType::deserialize(
        serde::de::value::StrDeserializer::<serde::de::value::Error>::new(value),
    )
    .ok()
}

/// One type name inside a `type` array. A bad name fails with the expected
/// shape and the offending value (serde's untagged derive would report only
/// "did not match any variant").
struct SchemaTypeName(SchemaType);

struct NameVisitor;

impl serde::de::Visitor<'_> for NameVisitor {
    type Value = SchemaTypeName;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON Schema type name")
    }

    fn visit_str<E: DeError>(self, value: &str) -> Result<Self::Value, E> {
        parse_type_name(value)
            .map(SchemaTypeName)
            .ok_or_else(|| E::invalid_value(serde::de::Unexpected::Str(value), &self))
    }
}

impl<'de> Deserialize<'de> for SchemaTypeName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NameVisitor)
    }
}

struct WireVisitor;

impl<'de> serde::de::Visitor<'de> for WireVisitor {
    type Value = SchemaTypeWire;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON Schema type name or an array of type names")
    }

    fn visit_str<E: DeError>(self, value: &str) -> Result<Self::Value, E> {
        parse_type_name(value)
            .map(SchemaTypeWire::Single)
            .ok_or_else(|| E::invalid_value(serde::de::Unexpected::Str(value), &self))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut types = Vec::new();
        while let Some(name) = seq.next_element::<SchemaTypeName>()? {
            types.push(name.0);
        }
        Ok(SchemaTypeWire::Multiple(types))
    }
}

impl<'de> Deserialize<'de> for SchemaTypeWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(WireVisitor)
    }
}

impl SchemaTypeWire {
    /// Returns the singular schema type plus whether the wire `type` DECLARED
    /// null (`type: [T, "null"]`).  The flag is a plain `bool` rather than an
    /// `Option<bool>` precisely because "the type array said not-null" is not a
    /// state this can produce - only "said null" or "said nothing" - so callers
    /// cannot be written against a third case that never occurs.
    pub(super) fn into_schema_type_and_nullable<E>(self) -> Result<(Option<SchemaType>, bool), E>
    where
        E: DeError,
    {
        match self {
            Self::Single(schema_type) => Ok((Some(schema_type), false)),
            Self::Multiple(schema_types) => Self::collapse_type_array(schema_types),
        }
    }

    /// The `type: [...]` arm of [`Self::into_schema_type_and_nullable`].
    fn collapse_type_array<E>(
        schema_types: Vec<SchemaType>,
    ) -> Result<(Option<SchemaType>, bool), E>
    where
        E: DeError,
    {
        let nullable = schema_types.contains(&SchemaType::Null);
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
        if schema_type.is_none() && nullable {
            return Ok((Some(SchemaType::Null), false));
        }
        Ok((schema_type, nullable))
    }
}

/// A schema in any position: the object form, or a JSON Schema 2020-12
/// boolean schema (`true` ≡ `{}`, `false` ≡ `{"not": {}}`, as in a
/// `components.schemas` / `$defs` entry).
///
/// Shares [`SchemaRef`]'s field-by-field visitor, so a [`Schema`] and an
/// inline [`SchemaRef`] read every keyword identically (including the
/// OpenAPI 3.0 boolean `exclusiveMinimum` / `exclusiveMaximum`); a pure
/// `{"$ref": ...}` object becomes a schema carrying only `ref_path`.
impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match SchemaRef::deserialize(deserializer)? {
            SchemaRef::Inline(schema) => *schema,
            SchemaRef::Ref(reference) => Self {
                ref_path: Some(reference.ref_path),
                ..Self::default()
            },
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

    /// `properties` … `propertyNames`.
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
        if let Some(value) = &self.property_names {
            out.serialize_field("propertyNames", value)?;
        }
        Ok(())
    }

    /// `enum` … `discriminator`.
    ///
    /// `nullable_ref` suppresses the explicit `anyOf` field: a nullable `$ref`
    /// already emitted its own `anyOf` in the head, and the validation at the
    /// top of [`Schema::serialize`] guarantees the two never both carry data.
    fn serialize_composition<S>(
        &self,
        out: &mut S,
        nullable: bool,
        nullable_ref: bool,
    ) -> Result<(), S::Error>
    where
        S: SerializeStruct,
    {
        if let Some(values) = &self.r#enum {
            // A nullable `enum` must name `null` or it still rejects it,
            // whatever `type` says.
            if nullable && !values.contains(&serde_json::Value::Null) {
                out.serialize_field("enum", &EnumWithNull(values))?;
            } else {
                out.serialize_field("enum", values)?;
            }
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
        if let Some(value) = self.deprecated {
            out.serialize_field("deprecated", &value)?;
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

/// `enum` values plus a trailing `null` (borrowing, no cloned `Vec`).
struct EnumWithNull<'a>(&'a [serde_json::Value]);

impl Serialize for EnumWithNull<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len() + 1))?;
        for value in self.0 {
            seq.serialize_element(value)?;
        }
        seq.serialize_element(&serde_json::Value::Null)?;
        seq.end()
    }
}

/// `schema` serialized as if it were not nullable (the first `anyOf` branch
/// of [`NullableComposition`]).
struct NonNullable<'a>(&'a Schema);

impl Serialize for NonNullable<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_as(serializer, false)
    }
}

/// `anyOf: [<schema>, {type: null}]` for a nullable schema (a `$ref` too)
/// whose composition keyword would otherwise still reject `null`.
struct NullableComposition<'a>(&'a Schema);

impl Serialize for NullableComposition<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&NonNullable(self.0))?;
        seq.serialize_element(&NullSchema)?;
        seq.end()
    }
}

impl Schema {
    /// Whether a composition keyword (`oneOf` / `allOf` / `not`, or an
    /// `anyOf` without a `{type: null}` branch) makes this schema reject
    /// `null` even when it is flagged `nullable`.
    ///
    /// Serialization only (deciding the `anyOf [.., {type: null}]` wrapper,
    /// which is always correct for a nullable schema): deserialization asks
    /// the finer [`Schema::composition_admits_null`].
    pub(super) fn composition_rejects_null(&self) -> bool {
        self.one_of.is_some()
            || self.all_of.is_some()
            || self.not.is_some()
            || self
                .any_of
                .as_deref()
                .is_some_and(|members| !has_null_branch(members))
    }

    /// Whether every composition keyword admits `null` — deciding whether a
    /// deserialized `type: [T, "null"]` keeps its `null`: an `anyOf` with a
    /// member that admits `null` (see [`admits_null`]), a `oneOf` with
    /// exactly one such member (the other branches are taken to reject
    /// `null`, the shape a nullable composition is written in; two admitting
    /// branches make `oneOf` reject it), an `allOf` whose every member admits
    /// `null`, and a `not` whose subschema provably rejects `null` (see
    /// [`rejects_null`]).  A schema without composition keywords trivially
    /// admits it.
    pub(super) fn composition_admits_null(&self) -> bool {
        self.any_of
            .as_deref()
            .is_none_or(|members| members.iter().any(admits_null))
            && self
                .one_of
                .as_deref()
                .is_none_or(|members| members.iter().filter(|m| admits_null(m)).count() == 1)
            && self
                .all_of
                .as_deref()
                .is_none_or(|members| members.iter().all(admits_null))
            && self.not.as_ref().is_none_or(rejects_null)
    }
}

/// Whether the composition member `member` admits `null`: an inline
/// `{type: null}`, a nullable schema (`type: [T, "null"]`, a nullable
/// `$ref`), or a type-less, `$ref`-less schema whose `enum` (if any) names
/// `null` and whose own composition admits it (`{}` included).  A plain
/// `$ref` is not resolvable here and counts as rejecting `null`.
fn admits_null(member: &SchemaRef) -> bool {
    let SchemaRef::Inline(schema) = member else {
        return false;
    };
    schema.nullable == Some(true)
        || schema.schema_type == Some(SchemaType::Null)
        || (schema.schema_type.is_none()
            && schema.ref_path.is_none()
            && schema
                .r#enum
                .as_ref()
                .is_none_or(|values| values.contains(&serde_json::Value::Null))
            && schema.composition_admits_null())
}

/// `{type: null}` (inline).
fn is_null_schema(member: &SchemaRef) -> bool {
    matches!(member, SchemaRef::Inline(schema) if schema.schema_type == Some(SchemaType::Null))
}

/// Whether one of `members` is `{type: null}`.
fn has_null_branch(members: &[SchemaRef]) -> bool {
    members.iter().any(is_null_schema)
}

/// Whether `schema` provably rejects `null`: an inline, non-nullable schema
/// with a non-`null` `type` or an `enum` without `null`.  A `$ref` (not
/// resolvable here) and anything else is not provable.
fn rejects_null(schema: &SchemaRef) -> bool {
    let SchemaRef::Inline(schema) = schema else {
        return false;
    };
    schema.nullable != Some(true)
        && (schema
            .schema_type
            .as_ref()
            .is_some_and(|ty| *ty != SchemaType::Null)
            || schema
                .r#enum
                .as_ref()
                .is_some_and(|values| !values.contains(&serde_json::Value::Null)))
}

impl Serialize for Schema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.serialize_as(serializer, self.nullable == Some(true))
    }
}

impl Schema {
    /// [`Serialize`] with `nullable` in place of `self.nullable`.
    ///
    /// OpenAPI 3.1 has no `nullable` keyword, so it is carried by the shape:
    /// `type: [T, "null"]`, `anyOf: [{$ref}, {type: null}]` for a reference,
    /// `null` appended to an `enum`, and — for a schema (a `$ref` too) whose
    /// `oneOf` / `allOf` / `not` / `anyOf` would reject `null`, whether or
    /// not it has a `type` (`type: [T, "null"]` cannot override a sibling
    /// composition keyword) — `anyOf: [<schema>, {type: null}]`.  Any other
    /// type-less schema already accepts `null` and is emitted as is.
    fn serialize_as<S>(&self, serializer: S, nullable: bool) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let nullable_ref = nullable && self.ref_path.is_some();
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
        // It runs before the composition wrapper below, so a nullable `$ref`
        // with a `oneOf` / `allOf` / `not` sibling is rejected the same way.
        if nullable_ref && self.schema_type.is_some() {
            return Err(serde::ser::Error::custom(
                "invalid Schema: nullable `$ref` serializes through anyOf and cannot also carry an explicit type; build it via Schema::nullable_reference",
            ));
        }
        // A nullable `$ref` with a sibling `oneOf` / `allOf` / `not` is
        // wrapped too: `anyOf: [{$ref}, {type: null}]` next to the keyword
        // would still reject `null`.  (With an explicit `any_of` it is the
        // error above — there is no slot left for the null branch.)
        if nullable
            && self.composition_rejects_null()
            && !(self.ref_path.is_some() && self.any_of.is_some())
        {
            let mut out = serializer.serialize_struct("Schema", 1)?;
            out.serialize_field("anyOf", &NullableComposition(self))?;
            return out.end();
        }
        let mut out = serializer.serialize_struct("Schema", 43)?;
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
            // per-schema `SchemaTypeWire` value. A nullable `null` (e.g.
            // `Option<()>`) stays the single `"null"` (never `["null","null"]`).
            if nullable && schema_type != SchemaType::Null {
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
        // Single-expression arms: a binding pattern on its own line gets no
        // coverage region on current rustc.
        match (&self.example, &self.examples) {
            (Some(example), Some(examples)) => out.serialize_field(
                "examples",
                &ExamplesWithLegacy {
                    example: Some(example),
                    examples,
                },
            )?,
            (Some(example), None) => out.serialize_field(
                "examples",
                &ExamplesWithLegacy {
                    example: Some(example),
                    examples: &[],
                },
            )?,
            (None, Some(examples)) => out.serialize_field("examples", examples)?,
            (None, None) => {}
        }
        // Order is load-bearing: these six calls reproduce the exact field
        // sequence of the previous straight-line body.
        self.serialize_numeric_constraints(&mut out)?;
        self.serialize_string_constraints(&mut out)?;
        self.serialize_array_constraints(&mut out)?;
        self.serialize_object_constraints(&mut out)?;
        self.serialize_composition(&mut out, nullable, nullable_ref)?;
        self.serialize_trailing_flags(&mut out)?;
        out.end()
    }
}
