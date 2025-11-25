use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

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

