use std::path::{Path, PathBuf};

use super::MANIFEST;

pub(super) fn resolve_manifest_path(explicit: Option<&str>) -> Result<(PathBuf, PathBuf), String> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(format!("manifest not found: {}", path.display()));
        }
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("manifest has no parent directory: {}", path.display()))?;
        return Ok((path, dir));
    }

    find_nearest_manifest(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .ok_or_else(|| {
            "could not find a harn.toml in the current directory or its parents".to_string()
        })
}

pub(super) fn find_nearest_manifest(start: &Path) -> Option<(PathBuf, PathBuf)> {
    const MAX_PARENT_DIRS: usize = 16;
    let mut cursor = Some(start.to_path_buf());
    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let base = if dir.is_dir() {
            dir
        } else {
            dir.parent()?.to_path_buf()
        };
        let candidate = base.join(MANIFEST);
        if candidate.is_file() {
            return Some((candidate, base));
        }
        if base.join(".git").exists() {
            break;
        }
        cursor = base.parent().map(Path::to_path_buf);
    }
    None
}

pub(super) fn secret_namespace_for(manifest_dir: &Path) -> String {
    match std::env::var("HARN_SECRET_NAMESPACE") {
        Ok(namespace) if !namespace.trim().is_empty() => namespace,
        _ => {
            let leaf = manifest_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace");
            format!("harn/{leaf}")
        }
    }
}
