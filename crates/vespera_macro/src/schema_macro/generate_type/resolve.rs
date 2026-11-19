//! Which struct a `schema_type!` source names: the `SCHEMA_STORAGE` entry
//! (no file I/O) when it is the requested one, else a file lookup.

use std::borrow::Cow;
use std::collections::HashMap;

use super::super::file_lookup::{
    LookupError, call_site_definition, call_site_module, find_struct_from_path_detailed,
    reexported_module, requested_module_path, storage_module_path,
};
use super::super::input::SchemaTypeInput;
use super::super::module_scope::{Resolved, module_scope, resolve_name};
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
    let resolved = resolve_found(input, source_type_name, schema_storage)?;
    if let (Ok(None), Some(defined_in)) = (requested_module_path(&input.source_type), &resolved.1) {
        let call_site = call_site_module();
        ensure_bare_name_in_scope(
            &input.source_type,
            source_type_name,
            defined_in,
            call_site.as_deref(),
        )?;
    }
    Ok(resolved)
}

/// [`resolve_source`] before the bare-name scope check.
fn resolve_found<'a>(
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
            // A qualified path with an entry of unknown module (outside `src/`)
            // is no proof of the requested item: the file lookup decides.
            (Some(_), None) => false,
            (None, _) => true,
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
            let lookup: Result<(StructMetadata, Vec<String>), _> = local.map_or_else(
                || find_struct_from_path_detailed(&input.source_type, schema_name_hint),
                Ok,
            );
            match (lookup, storage_hit) {
                (Ok((found, module_path)), _) => Ok((Cow::Owned(found), Some(module_path))),
                // An entry whose module is unknown stands in when the file
                // lookup finds nothing (the fallback the doc above describes).
                (Err(err), Some(stored))
                    if storage_module.is_none() && lookup_failure_falls_back(&err) =>
                {
                    Ok((Cow::Borrowed(stored), requested))
                }
                (Err(err), _) => Err(with_module_note(
                    err.to_syn_error(&input.source_type),
                    &input.source_type,
                    source_type_name,
                    storage_module.as_deref(),
                )),
            }
        }
    }
}

/// Whether a failed file lookup leaves an entry of unknown module standing in
/// for the source: only when no file of the requested module could be found
/// (or no manifest dir exists to look in). A path through another crate
/// (`LookupError::ExternalCrate`) is an error: a same-named local entry is
/// not that crate's item (R25-SM2-01).
fn lookup_failure_falls_back(err: &LookupError) -> bool {
    matches!(
        err,
        LookupError::NotFound { .. }
            | LookupError::BareNotFound { .. }
            | LookupError::MissingManifestDir
    )
}

/// A bare source name must be in scope at the call site (R24-SM2-F1): the
/// generated `impl From<Name>` and the other uses of the source type name it
/// as written. A struct of another module that the call site does not import
/// (or of an inline module) cannot be named there, so the call asks for a
/// path instead. Judged only when the call-site module is located.
fn ensure_bare_name_in_scope(
    source_type: &syn::Type,
    name: &str,
    defined_in: &[String],
    call_site: Option<&[String]>,
) -> syn::Result<()> {
    let Some(call_site) = call_site else {
        return Ok(());
    };
    if call_site == defined_in || module_scope(call_site).is_none() {
        return Ok(());
    }
    match resolve_name(&[name.to_string()], call_site) {
        Resolved::Path(_) => Ok(()),
        Resolved::Unknown => {
            let path = defined_in.join("::");
            Err(syn::Error::new_spanned(
                source_type,
                format!(
                    "schema_type!: `{name}` is defined in `{path}`, which this module does not import; name it with its full path (`{path}::{name}`) or import it"
                ),
            ))
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::metadata::StructMetadata;
    use crate::schema_macro::file_cache::bump_epoch;
    use crate::schema_macro::input::SchemaTypeInput;

    /// A temp crate holding `files` (paths relative to its root).
    fn crate_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    fn path(segments: &[&str]) -> Vec<String> {
        segments.iter().map(ToString::to_string).collect()
    }

    /// R24-SM2-F1: a bare source name the call site cannot name is refused
    /// (an inline module's struct, or one of another module the call site
    /// does not import); an import, a local definition or an unlocated call
    /// site are not.
    #[rstest::rstest]
    #[case::not_imported(
        "pub mod user { pub mod entity { pub struct Model; } }",
        &["crate", "user", "entity"],
        &["crate"],
        true
    )]
    #[case::imported(
        "pub mod user { pub mod entity { pub struct Model; } }\nuse user::entity::Model;",
        &["crate", "user", "entity"],
        &["crate"],
        false
    )]
    #[case::same_module("pub struct Model;", &["crate"], &["crate"], false)]
    #[case::unlocated_call_site(
        "pub mod user { pub mod entity { pub struct Model; } }",
        &["crate", "user", "entity"],
        &["crate", "gone"],
        false
    )]
    fn bare_source_names_must_be_in_scope(
        #[case] lib: &str,
        #[case] defined_in: &[&str],
        #[case] call_site: &[&str],
        #[case] refused: bool,
    ) {
        let dir = crate_with(&[("src/lib.rs", lib)]);
        let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        let source: syn::Type = syn::parse_str("Model").unwrap();
        let result =
            ensure_bare_name_in_scope(&source, "Model", &path(defined_in), Some(&path(call_site)));
        assert_eq!(result.is_err(), refused, "{result:?}");
        if let Err(error) = result {
            assert!(
                error.to_string().contains("crate::user::entity::Model"),
                "{error}"
            );
        }
    }

    #[test]
    fn no_call_site_judges_nothing() {
        let source: syn::Type = syn::parse_str("Model").unwrap();
        assert!(ensure_bare_name_in_scope(&source, "Model", &path(&["crate", "a"]), None).is_ok());
    }

    /// R24-SM2-F2: a stored entry of unknown module (outside `src/`) is no
    /// proof for a qualified path: the file lookup's `User` wins.
    #[test]
    fn unknown_storage_module_does_not_override_the_file_lookup() {
        let dir = crate_with(&[
            ("src/lib.rs", "pub mod models;"),
            ("src/models/mod.rs", "pub mod user;"),
            ("src/models/user.rs", "pub struct User { pub id: i32 }"),
        ]);
        let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        let legacy = StructMetadata::new(
            "User".to_string(),
            "pub struct User { pub legacy: String }".to_string(),
        );
        let storage = HashMap::from([("User".to_string(), legacy)]);
        let input: SchemaTypeInput =
            syn::parse_str("UserDto from crate::models::user::User").unwrap();
        let (found, module) = resolve_source(&input, "User", &storage).unwrap();
        assert!(found.definition.contains("id"), "{}", found.definition);
        assert_eq!(module, Some(path(&["crate", "models", "user"])));
    }

    /// R24-SM2-F2: the same entry stands in when the file lookup finds nothing.
    #[test]
    fn unknown_storage_module_is_the_fallback_when_the_lookup_fails() {
        let dir = crate_with(&[("src/lib.rs", "")]);
        let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        bump_epoch();
        let legacy = StructMetadata::new(
            "User".to_string(),
            "pub struct User { pub legacy: String }".to_string(),
        );
        let storage = HashMap::from([("User".to_string(), legacy)]);
        let input: SchemaTypeInput = syn::parse_str("UserDto from crate::nowhere::User").unwrap();
        let (found, module) = resolve_source(&input, "User", &storage).unwrap();
        assert!(found.definition.contains("legacy"), "{}", found.definition);
        assert_eq!(module, Some(path(&["crate", "nowhere"])));
    }

    /// R25-SM2-01: a source path through another crate (`entity::user::Model`,
    /// `entity` an `--extern` crate) is refused, not replaced by an entry of
    /// unknown module that happens to share the name.
    #[test]
    fn extern_crate_source_is_not_replaced_by_a_same_named_entry() {
        let dir = crate_with(&[("src/lib.rs", "pub struct Model;\n")]);
        let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        let _args = crate::env_access::test_env::set_var(
            crate::env_access::COMPILER_ARGS_KEY,
            "rustc\n--crate-name\napp\n--extern\nentity=/deps/libentity.rlib",
        );
        bump_epoch();
        let storage = HashMap::from([(
            "Model".to_string(),
            StructMetadata::new(
                "Model".to_string(),
                "pub struct Model { pub id: i32 }".to_string(),
            ),
        )]);
        let input: SchemaTypeInput = syn::parse_str("Dto from entity::user::Model").unwrap();
        let err = resolve_source(&input, "Model", &storage)
            .expect_err("an external-crate source must be an error");
        assert!(err.to_string().contains("another crate"), "{err}");
    }
}
