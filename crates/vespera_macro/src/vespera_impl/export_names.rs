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
    let file = proc_macro2::Span::call_site().local_file()?;
    let start = name.span().start();
    Some(ExportSite {
        file: file.display().to_string(),
        line: start.line,
        column: start.column,
    })
}

/// Claims `app` for the crate build `krate` at `site`.  Re-expanding the
/// same site (rust-analyzer) is fine, and a rename there frees the old name;
/// an earlier site whose file no longer mentions `app` is a stale IDE
/// leftover and is replaced.
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
        && still_mentions(existing, app)
    {
        return Err(format!(
            "export_app! macro: `{app}` is already exported by this crate ({}, line {}); every export_app! of a crate needs a distinct name (its OpenAPI sidecar, cache and export key are named `<crate>/{app}`)",
            Path::new(&existing.file).file_name().map_or_else(|| existing.file.clone(), |name| name.to_string_lossy().into_owned()),
            existing.line
        ));
    }
    sites.insert(key, site);
    Ok(())
}

/// Whether `site`'s file (still) mentions `app` — an unreadable one does not.
fn still_mentions(site: &ExportSite, app: &str) -> bool {
    crate::schema_macro::file_cache::get_file_content(Path::new(&site.file))
        .is_some_and(|content| content.contains(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(file: &Path, line: usize) -> Option<ExportSite> {
        Some(ExportSite {
            file: file.display().to_string(),
            line,
            column: 4,
        })
    }

    /// A second site of one name is rejected; the same site, another crate
    /// build, a stale site and a renamed site are not.
    #[test]
    fn duplicate_export_names_are_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "mod admin { export_app!(DupApp); }\nmod public { export_app!(DupApp); }").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let krate = format!("{}/dup", dir.path().display());

        assert_eq!(claim_export_name(&krate, "DupApp", None), Ok(()));
        assert_eq!(claim_export_name(&krate, "DupApp", site(&file, 1)), Ok(()));
        assert_eq!(claim_export_name(&krate, "DupApp", site(&file, 1)), Ok(()));
        let error = claim_export_name(&krate, "DupApp", site(&file, 2)).unwrap_err();
        assert!(error.contains("`DupApp` is already exported by this crate (lib.rs, line 1)"), "{error}");
        assert!(error.ends_with("named `<crate>/DupApp`)"), "{error}");
        assert_eq!(claim_export_name(&format!("{krate}.bin"), "DupApp", site(&file, 2)), Ok(()));

        // Renamed at line 1: the name is free for line 2.
        assert_eq!(claim_export_name(&krate, "Renamed", site(&file, 1)), Ok(()));
        assert_eq!(claim_export_name(&krate, "DupApp", site(&file, 2)), Ok(()));

        // A site whose file no longer exports the name is stale.
        let gone = dir.path().join("gone.rs");
        assert_eq!(claim_export_name(&krate, "Other", site(&gone, 1)), Ok(()));
        assert_eq!(claim_export_name(&krate, "Other", site(&file, 3)), Ok(()));
    }
}
