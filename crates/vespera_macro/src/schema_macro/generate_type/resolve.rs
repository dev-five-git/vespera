//! Which struct a `schema_type!` source names: the `SCHEMA_STORAGE` entry
//! (no file I/O) when it is the requested one, else a file lookup.

use std::borrow::Cow;
use std::collections::HashMap;

use super::super::file_lookup::{
    call_site_definition, find_struct_from_path_detailed, reexported_module, requested_module_path,
    storage_module_path,
};
use super::super::input::SchemaTypeInput;
use crate::metadata::StructMetadata;

/// The source struct and the module it is defined in.
///
/// The storage-then-file-lookup resolution is identical for qualified
/// (`crate::models::user::Model`) and simple (`Model`) source paths, and
/// `find_struct_from_path_detailed` runs at most once (a miss used to re-run
/// the whole candidate scan just to surface the error).
///
/// The module path is the defining file's (overriding the path as written:
/// `models::user::User` / `super::User` would re-root every field type and
/// serde path against it), so relative relation paths like
/// `super::user::Entity` resolve correctly.
pub(super) fn resolve_source<'a>(
    input: &SchemaTypeInput,
    source_type_name: &str,
    schema_storage: &'a HashMap<String, StructMetadata>,
) -> syn::Result<(Cow<'a, StructMetadata>, Option<Vec<String>>)> {
    let schema_name_hint = input.schema_name.as_deref();
    // Storage is keyed by the bare struct name, so a qualified source path
    // only trusts a storage entry whose defining file belongs to the
    // requested module (`crate::a::User` must not pick `crate::b::User`'s
    // derive); otherwise the file lookup decides, the storage entry being the
    // fallback when the file lookup cannot find the struct either.
    let storage_hit = super::super::storage_entry(schema_storage, source_type_name);
    let storage_module = storage_hit.and_then(storage_module_path);
    // A path through another crate's import is an error: read as a bare
    // name, a same-named local `#[derive(Schema)]` used to be taken.
    let requested = requested_module_path(&input.source_type)
        .map_err(|err| err.to_syn_error(&input.source_type))?;
    // Same-named items of several modules (one stored definition): a bare
    // name is the variant of the module it resolves to at the call site (a
    // `use crate::first::Page;` read the last-derived `second::Page`).
    let variant = storage_hit
        .filter(|_| requested.is_none())
        .and_then(|stored| {
            let module = super::super::bare_name_module(&input.source_type)?;
            crate::schema_impl::variants::in_module(&stored.name, &module)
                .map(|found| (found, module))
        });
    if let Some((found, module)) = variant {
        return Ok((Cow::Owned(found), Some(module)));
    }
    // A bare name resolves in the call-site file: a definition there wins
    // over a same-named `#[derive(Schema)]` another file registered.
    let local = storage_hit
        .filter(|_| requested.is_none())
        .and_then(|stored| call_site_definition(stored, &input.source_type, schema_name_hint));
    let storage_trusted = local.is_none()
        && match (&requested, &storage_module) {
            // A re-export (`pub use user::User;` in `models`) names the same item.
            (Some(requested), Some(defined_in)) => {
                requested == defined_in
                    || reexported_module(requested, source_type_name).as_ref() == Some(defined_in)
            }
            _ => true,
        };
    match storage_hit {
        Some(found) if storage_trusted => {
            // The defining file's module, else the absolute requested one
            // (`None`: the path as written).
            Ok((Cow::Borrowed(found), storage_module.or(requested)))
        }
        _ => {
            // A same-named schema of ANOTHER module is never a fallback
            // (it used to be used silently).
            let (found, module_path) = local
                .map_or_else(
                    || find_struct_from_path_detailed(&input.source_type, schema_name_hint),
                    Ok,
                )
                .map_err(|err| {
                    with_module_note(
                        err.to_syn_error(&input.source_type),
                        &input.source_type,
                        source_type_name,
                        storage_module.as_deref(),
                    )
                })?;
            Ok((Cow::Owned(found), Some(module_path)))
        }
    }
}

/// `error`, plus a note naming the module a same-named schema lives in.
fn with_module_note(
    mut error: syn::Error,
    source_type: &syn::Type,
    source_type_name: &str,
    storage_module: Option<&[String]>,
) -> syn::Error {
    if let Some(module) = storage_module {
        error.combine(syn::Error::new_spanned(
            source_type,
            format!(
                "note: `{source_type_name}` exists in `{}`, which is not the requested module",
                module.join("::")
            ),
        ));
    }
    error
}
