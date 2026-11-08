//! The storage layout shared by snapshot owners and branch execution.

use std::path::{Path, PathBuf};

/// One working copy and the pool containing its independently owned storage.
/// Both paths derive from the storage root; callers cannot narrow the pool
/// while keeping the same working copy.
#[derive(Clone, Debug)]
pub struct ReplayWorkspaceLocation {
    workspace_root: PathBuf,
    storage_pool: PathBuf,
}

impl ReplayWorkspaceLocation {
    /// The working-copy directory inside independently owned storage.
    pub const DIRECTORY_NAME: &str = "workspace";

    /// Bind a canonical storage directory to its working copy and pool.
    /// Snapshot owners supply their storage root, never an execution's cwd.
    pub fn from_storage_root(root: &Path) -> std::io::Result<Self> {
        if !root.is_absolute()
            || root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "replay storage root must be an absolute normalized path",
            ));
        }
        let storage_root = root.canonicalize()?;
        if !storage_root.is_dir() || storage_root.parent().is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "replay storage root must be a directory inside its storage pool",
            ));
        }
        let storage_pool = storage_root.parent().expect("validated pool").to_path_buf();
        Ok(Self {
            workspace_root: storage_root.join(Self::DIRECTORY_NAME),
            storage_pool,
        })
    }

    /// The private working copy, which must exist before execution starts.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// The entire pool, including peers created after execution starts.
    pub fn storage_pool(&self) -> &Path {
        &self.storage_pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_storage_location_binds_the_copy_to_the_whole_pool() {
        let pool = tempfile::tempdir().unwrap();
        let pool_root = pool.path().canonicalize().unwrap();
        let mut locations = Vec::new();
        for arm in ["left", "right"] {
            let storage = pool.path().join(arm);
            std::fs::create_dir(&storage).unwrap();
            let location = ReplayWorkspaceLocation::from_storage_root(&storage).unwrap();
            assert_eq!(
                location.workspace_root(),
                storage.canonicalize().unwrap().join("workspace")
            );
            assert_eq!(location.storage_pool(), pool_root);
            locations.push(location);
        }
        assert_ne!(locations[0].workspace_root(), locations[1].workspace_root());
        assert!(ReplayWorkspaceLocation::from_storage_root(Path::new("relative")).is_err());
        assert!(ReplayWorkspaceLocation::from_storage_root(&pool.path().join("left/..")).is_err());
    }
}
