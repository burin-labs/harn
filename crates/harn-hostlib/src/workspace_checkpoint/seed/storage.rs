use super::*;

pub(super) struct PrivateStorage {
    temporary: Option<tempfile::TempDir>,
    path: PathBuf,
}

impl PrivateStorage {
    pub(super) fn new(temporary: tempfile::TempDir) -> Self {
        Self {
            path: temporary.path().to_path_buf(),
            temporary: Some(temporary),
        }
    }

    pub(super) fn retained(path: PathBuf) -> Self {
        Self {
            temporary: None,
            path,
        }
    }

    pub(super) fn retain(&mut self) {
        if let Some(temporary) = self.temporary.take() {
            self.path = temporary.keep();
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateStorage {
    fn drop(&mut self) {
        if self.temporary.is_some() {
            if let Ok(directory) = open_dir(self.path()) {
                let _ = make_removable(&directory);
            }
        }
    }
}

pub(super) fn remove_child(parent: &Dir, path: &Path) -> Result<(), SeedError> {
    let metadata = match parent.symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io("inspect retired copy", path, error)),
    };
    if metadata.is_dir() {
        make_child_removable(parent, path)
            .map_err(|error| io("prepare retired copy for removal", path, error))?;
        parent.remove_dir_all(path)
    } else {
        parent.remove_file(path)
    }
    .map_err(|error| io("remove retired copy", path, error))
}

pub(super) fn remove_abandoned_staging(parent: &Dir) -> Result<(), SeedError> {
    for entry in parent
        .entries()
        .map_err(|error| io("enumerate storage", Path::new("."), error))?
    {
        let entry = entry.map_err(|error| io("enumerate storage", Path::new("."), error))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("restore-"))
        {
            remove_child(parent, Path::new(&entry.file_name()))?;
        }
    }
    Ok(())
}

/// Only called on owned private trees after their execution has stopped.
pub(super) fn make_removable(directory: &Dir) -> std::io::Result<()> {
    directory.set_permissions(
        ".",
        removable_permissions(directory.dir_metadata()?.permissions()),
    )?;
    for entry in directory.entries()? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_child_removable(directory, Path::new(&entry.file_name()))?;
        }
        #[cfg(windows)]
        if entry.file_type()?.is_file() {
            let mut permissions = entry.metadata()?.permissions();
            permissions.set_readonly(false);
            directory.set_permissions(entry.file_name(), permissions)?;
        }
    }
    Ok(())
}

pub(super) fn make_child_removable(parent: &Dir, path: &Path) -> std::io::Result<()> {
    let metadata = parent.symlink_metadata(path)?;
    if metadata.is_dir() {
        let permissions = removable_permissions(metadata.permissions());
        if let Err(error) = parent.set_permissions(path, permissions.clone()) {
            #[cfg(unix)]
            restore_child_access(parent, path, permissions, error)?;
            #[cfg(not(unix))]
            return Err(error);
        }
        make_removable(&parent.open_dir(path)?)?;
    }
    Ok(())
}

#[cfg(unix)]
fn restore_child_access(
    parent: &Dir,
    path: &Path,
    permissions: Permissions,
    original: std::io::Error,
) -> std::io::Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(original);
    }
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| original)?;
    // cap-std needs a readable/writable handle for chmod. A stopped trial can
    // remove both rights. Operate on one owned child without following links.
    let result = unsafe {
        libc::fchmodat(
            parent.as_raw_fd(),
            name.as_ptr(),
            cap_std::fs::PermissionsExt::mode(&permissions) as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn removable_permissions(mut permissions: Permissions) -> Permissions {
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o700);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    permissions
}
