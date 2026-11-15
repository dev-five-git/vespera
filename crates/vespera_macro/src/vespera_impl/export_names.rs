//! One `export_app!` name per crate.
//!
//! An export's spec sidecar (`export/<crate>/<App>.openapi.json`), its
//! `include_str!` stem, its build cache and its `__VESPERA_EXPORT_KEY` are all
//! keyed by `<crate>/<App>`: two `export_app!(App)` in different modules of
//! one crate (`mod admin` / `mod public`) overwrote each other's spec, so a
//! parent merged the wrong one, and each pruned the other's `include_str!`
//! target.  The second one is a compile error instead.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError};

/// Where one `export_app!` was expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExportSite {
    pub(super) file: String,
    pub(super) line: usize,
    pub(super) column: usize,
}

/// `(crate build, App)` → the site that exported it.
type Sites = HashMap<(String, String), ExportSite>;

static EXPORT_SITES: LazyLock<Mutex<Sites>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The current expansion's site: the call-site file and the name's position
/// (`None` outside the compiler, where spans carry no file).
pub(super) fn current_site(name: &syn::Ident) -> Option<ExportSite> {
    proc_macro2::Span::call_site()
        .local_file()
        .map(|file| site_in(&file, name))
}

/// `name`'s position in `file`.
fn site_in(file: &Path, name: &syn::Ident) -> ExportSite {
    let start = name.span().start();
    ExportSite {
        file: file.display().to_string(),
        line: start.line,
        column: start.column,
    }
}

/// Claims `app` for the crate build `krate` at `site`.  Re-expanding the
/// same site (rust-analyzer) is fine, and a rename there frees the old name;
/// an earlier site whose position no longer holds `app` (the file changed,
/// or the line moved) is a stale IDE leftover and is replaced.
pub(super) fn claim_export_name(
    krate: &str,
    app: &str,
    site: Option<ExportSite>,
) -> Result<(), String> {
    let Some(site) = site else {
        return Ok(());
    };
    let mut sites = EXPORT_SITES.lock().unwrap_or_else(PoisonError::into_inner);
    sites.retain(|(k, a), s| !(k == krate && *s == site && a != app));
    let key = (krate.to_string(), app.to_string());
    if let Some(existing) = sites.get(&key)
        && *existing != site
        && still_exports(existing, app)
    {
        return Err(format!(
            "export_app! macro: `{app}` is already exported by this crate ({}, line {}); every export_app! of a crate needs a distinct name (its OpenAPI sidecar, cache and export key are named `<crate>/{app}`)",
            Path::new(&existing.file).file_name().map_or_else(
                || existing.file.clone(),
                |name| name.to_string_lossy().into_owned()
            ),
            existing.line
        ));
    }
    sites.insert(key, site);
    Ok(())
}

/// Whether `site`'s position (still) holds the name `app` — an unreadable
/// file does not.  Inserting a line above an `export_app!` moves it: the old
/// site then holds something else, so it no longer claims the name.
fn still_exports(site: &ExportSite, app: &str) -> bool {
    crate::schema_macro::file_cache::get_file_content(Path::new(&site.file)).is_some_and(
        |content| {
            let Some(line) = site
                .line
                .checked_sub(1)
                .and_then(|index| content.lines().nth(index))
            else {
                return false;
            };
            let rest: String = line.chars().skip(site.column).collect();
            rest.strip_prefix(app).is_some_and(|after| {
                !after.starts_with(|ch: char| ch.is_alphanumeric() || ch == '_')
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outside the compiler the call site has no file: no site.
    #[test]
    fn sites_come_from_the_call_site_file() {
        let name = syn::Ident::new("App", proc_macro2::Span::call_site());
        assert_eq!(current_site(&name), None);
        let site = site_in(Path::new("src/lib.rs"), &name);
        assert_eq!(site.file, "src/lib.rs");
    }

    /// The site of the name after `export_app!(` on `line` of `file` (column
    /// 0 when there is none).
    fn site(file: &Path, line: usize) -> ExportSite {
        let column = std::fs::read_to_string(file)
            .ok()
            .and_then(|content| {
                let text = content.lines().nth(line - 1)?.to_owned();
                let byte = text.find("export_app!(")? + "export_app!(".len();
                Some(text[..byte].chars().count())
            })
            .unwrap_or(0);
        let file = file.display().to_string();
        ExportSite { file, line, column }
    }

    /// Inserting a line above an `export_app!` (rust-analyzer re-expands it
    /// one line lower) is no duplicate: the old position no longer holds the
    /// name.  Another export of the same name still is.
    #[test]
    fn a_moved_export_frees_its_old_site() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        let krate = format!("{}/moved", dir.path().display());
        std::fs::write(&file, "export_app!(MovedApp);\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(
            claim_export_name(&krate, "MovedApp", Some(site(&file, 1))),
            Ok(())
        );
        std::fs::write(&file, "// inserted\nexport_app!(MovedApp);\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(
            claim_export_name(&krate, "MovedApp", Some(site(&file, 2))),
            Ok(())
        );
        // A line that no longer exists, or a longer name at the old
        // position, is not this export either.
        let at = |line, column| ExportSite {
            file: file.display().to_string(),
            line,
            column,
        };
        std::fs::write(&file, "export_app!(MovedApp);\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(
            claim_export_name(&krate, "MovedApp", Some(at(1, 12))),
            Ok(())
        );
        std::fs::write(&file, "export_app!(MovedAppV2);\nexport_app!(MovedApp);\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(
            claim_export_name(&krate, "MovedApp", Some(at(2, 12))),
            Ok(())
        );
        assert!(!still_exports(&at(0, 0), "MovedApp"));
        assert!(!still_exports(&at(9, 0), "MovedApp"));
        // A real second export (both positions hold the name) is rejected.
        std::fs::write(&file, "export_app!(MovedApp);\nexport_app!(MovedApp);\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        assert!(claim_export_name(&krate, "MovedApp", Some(at(1, 12))).is_err());
    }

    /// A second site of one name is rejected; the same site, another crate
    /// build, a stale site and a renamed site are not.
    #[test]
    fn duplicate_export_names_are_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(
            &file,
            "mod admin { export_app!(DupApp); }\nmod public { export_app!(DupApp); }",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let krate = format!("{}/dup", dir.path().display());

        assert_eq!(claim_export_name(&krate, "DupApp", None), Ok(()));
        assert_eq!(
            claim_export_name(&krate, "DupApp", Some(site(&file, 1))),
            Ok(())
        );
        assert_eq!(
            claim_export_name(&krate, "DupApp", Some(site(&file, 1))),
            Ok(())
        );
        let error = claim_export_name(&krate, "DupApp", Some(site(&file, 2))).unwrap_err();
        assert!(
            error.contains("`DupApp` is already exported by this crate (lib.rs, line 1)"),
            "{error}"
        );
        assert!(error.ends_with("named `<crate>/DupApp`)"), "{error}");
        assert_eq!(
            claim_export_name(&format!("{krate}.bin"), "DupApp", Some(site(&file, 2))),
            Ok(())
        );

        // Renamed at line 1: the name is free for line 2.
        assert_eq!(
            claim_export_name(&krate, "Renamed", Some(site(&file, 1))),
            Ok(())
        );
        assert_eq!(
            claim_export_name(&krate, "DupApp", Some(site(&file, 2))),
            Ok(())
        );

        // A site whose file no longer exports the name is stale.
        let gone = dir.path().join("gone.rs");
        assert_eq!(
            claim_export_name(&krate, "Other", Some(site(&gone, 1))),
            Ok(())
        );
        assert_eq!(
            claim_export_name(&krate, "Other", Some(site(&file, 3))),
            Ok(())
        );
    }
}
