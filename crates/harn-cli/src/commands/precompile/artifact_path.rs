use std::path::{Path, PathBuf};

/// Map a source path under `source_root` to its artifact destination.
///
/// Adjacent artifacts move with their source. An explicit output root mirrors
/// the source tree so equally named modules do not collide.
pub(super) fn output_path(
    source_path: &Path,
    source_root: Option<&Path>,
    out_root: Option<&Path>,
    extension: &str,
) -> Result<PathBuf, String> {
    let stem = source_path
        .file_stem()
        .ok_or_else(|| format!("source has no file stem: {}", source_path.display()))?;
    let Some(out_root) = out_root else {
        let parent = source_path.parent().unwrap_or_else(|| Path::new(""));
        let mut adjacent = parent.join(stem);
        adjacent.set_extension(extension);
        return Ok(adjacent);
    };
    let relative = match source_root {
        Some(root) => {
            let canonical = source_path
                .canonicalize()
                .unwrap_or_else(|_| source_path.to_path_buf());
            canonical
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| {
                    PathBuf::from(source_path.file_name().unwrap_or(source_path.as_os_str()))
                })
        }
        None => PathBuf::from(
            source_path
                .file_name()
                .ok_or_else(|| format!("source has no file name: {}", source_path.display()))?,
        ),
    };
    let mut dest = out_root.join(&relative);
    dest.set_extension(extension);
    Ok(dest)
}
