//! The type idents a documented type names: the reference graph
//! `compute_input_names` follows to find the types whose request view differs
//! transitively. A sidecar of `request_view`.

use std::collections::BTreeSet;

use super::super::{
    schema_attrs::extract_schema_constraints,
    scoped_ident::lookup_key,
    serde_attrs::{SerdeSide, entity_candidates},
    wire_override::container_override_type,
};
use crate::schema_macro::type_utils::option_inner;

/// Every type ident named by the item's fields (generic arguments included),
/// their `#[schema(value_type = ..)]` types, the entity components of their
/// relations (see [`add_relation_targets`]), and the container's `value_type` /
/// `into` / `from` / `try_from` types. `known` holds every component name.
pub(super) fn referenced_idents(item: &syn::Item, known: &BTreeSet<String>) -> BTreeSet<String> {
    let mut idents = BTreeSet::new();
    let mut add_fields = |fields: &syn::Fields| {
        for field in fields {
            collect_idents(&field.ty, &mut idents);
            add_relation_targets(&field.ty, known, &mut idents);
            if let Some(value_type) = extract_schema_constraints(&field.attrs).value_type {
                collect_idents(&value_type.0, &mut idents);
            }
        }
    };
    let attrs = match item {
        syn::Item::Struct(item) => {
            add_fields(&item.fields);
            &item.attrs
        }
        syn::Item::Enum(item) => {
            item.variants.iter().for_each(|v| add_fields(&v.fields));
            &item.attrs
        }
        _ => return idents,
    };
    for side in [SerdeSide::Serialize, SerdeSide::Deserialize] {
        if let Some(ty) = container_override_type(attrs, side) {
            collect_idents(&ty, &mut idents);
        }
    }
    idents
}

pub(super) fn collect_idents(ty: &syn::Type, idents: &mut BTreeSet<String>) {
    struct Visitor<'a>(&'a mut BTreeSet<String>);
    impl<'ast> syn::visit::Visit<'ast> for Visitor<'_> {
        /// Every segment ident; a module-qualified type (`b::Foo`) as its
        /// registry key instead of the bare ident, so it resolves to the
        /// declaration its path names.
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if let Some(last) = path.segments.last() {
                self.0
                    .insert(lookup_key(path).unwrap_or_else(|| last.ident.to_string()));
            }
            for segment in path.segments.iter().rev().skip(1) {
                self.0.insert(segment.ident.to_string());
            }
            syn::visit::visit_path(self, path);
        }
    }
    syn::visit::Visit::visit_type(&mut Visitor(idents), ty);
}

/// The component a `HasOne` / `BelongsTo` / `HasMany` field documents its
/// `<module>::Entity` as: the first candidate that is a known component (see
/// `entity_target`, which emits the same `$ref`). The request view of the type
/// with that relation differs when its target's does (the target's input is
/// referenced from the parent's request view). Other candidates are not
/// referenced: `$ref` never names them.
fn add_relation_targets(ty: &syn::Type, known: &BTreeSet<String>, idents: &mut BTreeSet<String>) {
    if let syn::Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
        && matches!(
            segment.ident.to_string().as_str(),
            "HasOne" | "BelongsTo" | "HasMany"
        )
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(entity)) = args.args.first()
        && let Some(target) = entity_candidates(option_inner(entity).unwrap_or(entity))
            .and_then(|names| names.into_iter().find(|name| known.contains(name)))
    {
        idents.insert(target);
    }
}
