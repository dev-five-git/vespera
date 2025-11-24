use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::route::extract_route_info;

pub fn collect_files(folder_path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder_path)
        .with_context(|| format!("Failed to read directory: {}", folder_path.display()))?
    {
        let entry = entry.with_context(|| "Failed to read directory entry")?;
        let path = entry.path();
        if path.is_file() {
            files.push(folder_path.join(path));
        } else if path.is_dir() {
            files.extend(collect_files(&folder_path.join(&path))?);
        }
    }
    Ok(files)
}

pub fn file_to_segments(file: &Path, base_path: &Path) -> Vec<String> {
    let file_stem = if let Ok(file_stem) = file.strip_prefix(base_path) {
        file_stem.display().to_string()
    } else {
        file.display().to_string()
    };
    let file_stem = file_stem.replace(".rs", "").replace("\\", "/");
    let mut segments: Vec<String> = file_stem
        .split("/")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if let Some(last) = segments.last()
        && last == "mod"
    {
        segments.pop();
    }
    segments
}

pub fn get_function_list(
    file: &Path,
    route_path: &str,
) -> Result<Vec<(syn::Ident, String, Option<String>)>> {
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read file: {}", file.display()))?;

    let file_ast = syn::parse_file(&content)
        .with_context(|| format!("Failed to parse file: {}", file.display()))?;

    let fn_list = file_ast
        .items
        .iter()
        .filter_map(|item| {
            if let syn::Item::Fn(fn_item) = item {
                // Extract route info (method and path) from attributes
                let route_info = extract_route_info(&fn_item.attrs);
                if let Some(route_info) = route_info {
                    Some((
                        fn_item.sig.ident.clone(),
                        route_info.method,
                        route_info.path.map(|p| {
                            format!("{}", vec![route_path, p.trim_start_matches('/')].join("/"))
                        }),
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect::<Vec<(syn::Ident, String, Option<String>)>>();
    Ok(fn_list)
}
