use super::*;

const LEASE_FILE: &str = "workspace.lease";

/// Save this descriptor with the owning run, in private product storage.
/// It binds a retained directory to the seed identity and capture limits.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReplayWorkspace {
    storage_root: PathBuf,
    receipt: SeedReceipt,
    limits: SeedLimits,
}

impl SavedReplayWorkspace {
    /// Directory retained by the snapshot owner, outside the working copy.
    pub fn storage_path(&self) -> &Path {
        &self.storage_root
    }

    /// Expected identity, checked before every restored working copy is exposed.
    pub fn receipt(&self) -> &SeedReceipt {
        &self.receipt
    }
}

impl ReplayWorkspace {
    /// Retain the seed after this handle closes. The returned descriptor must
    /// be saved with the owning run; retaining the directory does not save that
    /// run's metadata. The current execution keeps its exclusive lease.
    pub fn persist(&mut self) -> SavedReplayWorkspace {
        self.storage.retain();
        SavedReplayWorkspace {
            storage_root: self.storage.path().to_path_buf(),
            receipt: self.receipt.clone(),
            limits: self.limits,
        }
    }

    /// Acquire a saved workspace and restore its seed for a new trial. Refuses
    /// an active owner or changed seed before replacing the working copy.
    /// The prior execution and its children must have stopped before its owner
    /// releases the lease. Windows requires the original user-private parent ACL.
    pub fn restore_saved(saved: &SavedReplayWorkspace) -> Result<Self, SeedError> {
        let root = &saved.storage_root;
        let metadata = std::fs::symlink_metadata(root)
            .map_err(|error| io("inspect saved storage", root, error))?;
        if !metadata.is_dir()
            || root
                .canonicalize()
                .map_err(|error| io("resolve saved storage", root, error))?
                != *root
        {
            return Err(SeedError::Unsupported {
                path: root.clone(),
                reason: "saved storage must remain at its original canonical directory",
            });
        }
        #[cfg(unix)]
        if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
            return Err(SeedError::Unsupported {
                path: root.clone(),
                reason: "saved storage is no longer private to its owner",
            });
        }
        let parent = open_dir(root)?;
        let held = lease(&parent, root, false)?;
        let mut workspace = Self {
            _lease: held,
            storage: storage::PrivateStorage::retained(root.clone()),
            workspace: root.join("workspace"),
            receipt: saved.receipt.clone(),
            limits: saved.limits,
        };
        workspace.restore()?;
        Ok(workspace)
    }
}

pub(super) fn lease(parent: &Dir, root: &Path, create: bool) -> Result<std::fs::File, SeedError> {
    if !create {
        let metadata = parent
            .symlink_metadata(LEASE_FILE)
            .map_err(|error| io("inspect lease", Path::new(LEASE_FILE), error))?;
        if !metadata.is_file() {
            return Err(SeedError::Unsupported {
                path: root.join(LEASE_FILE),
                reason: "workspace lease must be its original regular file",
            });
        }
    }
    let file = parent
        .open_with(
            LEASE_FILE,
            cap_std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(create),
        )
        .map_err(|error| io("open workspace lease", root, error))?
        .into_std();
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(SeedError::InUse {
            path: root.to_path_buf(),
        }),
        Err(std::fs::TryLockError::Error(error)) => Err(io("lock workspace", root, error)),
    }
}

pub(super) fn require_directory(parent: &Dir, name: &str) -> Result<(), SeedError> {
    let metadata = parent
        .symlink_metadata(name)
        .map_err(|error| io("inspect directory", Path::new(name), error))?;
    if !metadata.is_dir() {
        return Err(SeedError::Unsupported {
            path: PathBuf::from(name),
            reason: "the captured seed must remain a directory, not a symbolic link",
        });
    }
    Ok(())
}
