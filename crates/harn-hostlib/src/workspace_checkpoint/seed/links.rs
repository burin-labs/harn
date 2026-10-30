use super::{io, Dir, Entry, Path, SeedError};

pub(super) fn copy(
    source: &Dir,
    destination: Option<&Dir>,
    path: &Path,
) -> Result<Entry, SeedError> {
    let target = source
        .read_link_contents(path)
        .map_err(|error| io("read symbolic link", path, error))?;
    if target.is_absolute() || target.to_str().is_none() {
        return Err(SeedError::Unsupported {
            path: path.to_path_buf(),
            reason: "symbolic links must have a relative UTF-8 target inside the workspace",
        });
    }
    // Resolve through the open capability directory: lexical normalization
    // alone cannot prove confinement when a target crosses another link.
    source
        .canonicalize(path)
        .map_err(|_| SeedError::Unsupported {
            path: path.to_path_buf(),
            reason: "symbolic links must resolve inside the captured workspace",
        })?;
    let metadata = source
        .metadata(path)
        .map_err(|error| io("inspect link target", path, error))?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(SeedError::Unsupported {
            path: path.to_path_buf(),
            reason: "symbolic links must resolve to a regular file or directory",
        });
    }
    let directory = metadata.is_dir();
    if let Some(destination) = destination {
        #[cfg(not(windows))]
        let result = destination.symlink(&target, path);
        #[cfg(windows)]
        let result = if directory {
            destination.symlink_dir(&target, path)
        } else {
            destination.symlink_file(&target, path)
        };
        result.map_err(|error| io("create symbolic link", path, error))?;
    }
    Ok(Entry::Link {
        target: target
            .to_str()
            .expect("the link target was checked")
            .to_owned(),
        directory,
    })
}
