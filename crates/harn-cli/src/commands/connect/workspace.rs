use std::path::{Path, PathBuf};

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

/// Locate the nearest `harn.toml` via the shared project-root walk.
/// Returns `(manifest_path, manifest_dir)`.
pub(super) fn find_nearest_manifest(start: &Path) -> Option<(PathBuf, PathBuf)> {
    let found = harn_modules::manifest_walk::find_nearest_manifest(start)?;
    Some((found.path, found.dir))
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
