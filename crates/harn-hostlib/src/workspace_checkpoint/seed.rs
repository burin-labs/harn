//! A private initial workspace and repeatable copies at one stable path.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_std::fs::{Dir, Permissions};
use serde::Serialize;
use sha2::{Digest, Sha256};

mod links;
mod storage;

/// Resource ceilings checked while reading, including ignored files.
#[derive(Clone, Copy, Debug)]
pub struct SeedLimits {
    /// Files and directories, excluding the root itself.
    pub max_entries: usize,
    /// Total regular-file and symbolic-link content in each copy.
    pub max_bytes: u64,
    /// Content limit for any single regular file or symbolic link.
    pub max_file_bytes: u64,
    /// Maximum number of relative path components.
    pub max_depth: usize,
}

impl Default for SeedLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_bytes: 1 << 30,
            max_file_bytes: 128 << 20,
            max_depth: 128,
        }
    }
}

/// Identity and measured size of the captured regular files and directories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SeedReceipt {
    /// Versioned receipt contract.
    pub schema: &'static str,
    /// Digest of sorted paths, contents, links and file/directory permissions.
    pub sha256: String,
    /// Regular files actually read.
    pub files: usize,
    /// Files and directories actually observed.
    pub entries: usize,
    /// Regular-file and symbolic-link bytes actually read.
    pub bytes: u64,
}

/// A capture or restore that cannot supply a verified working copy.
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    /// The source exceeded a caller-supplied ceiling.
    #[error("workspace seed exceeds {resource}: measured {observed}, limit {limit}")]
    Limit {
        /// Measured resource, such as total bytes or entries.
        resource: &'static str,
        /// Non-null measurement that exceeded the ceiling.
        observed: u64,
        /// Caller-supplied ceiling.
        limit: u64,
    },
    /// An entry cannot be copied without weakening the snapshot contract.
    #[error("workspace seed cannot include {path:?}: {reason}")]
    Unsupported {
        /// Entry or storage path that prevents capture.
        path: PathBuf,
        /// Required correction.
        reason: &'static str,
    },
    /// A second source read did not match the captured tree.
    #[error("workspace changed while capturing its seed; stop its writers and try again")]
    SourceChanged,
    /// Private seed contents no longer match the recorded identity.
    #[error("workspace seed no longer matches its captured identity; the current workspace was not replaced")]
    SeedChanged,
    /// An underlying filesystem operation failed.
    #[error("workspace seed {operation} {path:?}: {source}")]
    Io {
        /// Operation that failed, including whether replacement already occurred.
        operation: &'static str,
        /// Affected path, relative to its open directory where applicable.
        path: PathBuf,
        /// Original filesystem error.
        source: std::io::Error,
    },
}

/// Owns a private seed and a working copy. Restore only after the previous
/// execution and its child processes have stopped; cancellation belongs to
/// the session owner. Dropping this value retires both directories.
pub struct ReplayWorkspace {
    storage: storage::PrivateStorage,
    workspace: PathBuf,
    receipt: SeedReceipt,
    limits: SeedLimits,
}

impl ReplayWorkspace {
    /// Capture all regular files and internal relative links, including ignored
    /// and untracked files. External links and special files refuse. Directory
    /// structure and permissions are retained, not directory timestamps or ACLs.
    pub fn capture(source: &Path, limits: SeedLimits) -> Result<Self, SeedError> {
        Self::capture_in(source, &std::env::temp_dir(), limits)
    }

    /// Place private storage under an existing directory outside the source.
    /// On Windows the caller must supply a parent whose ACL restricts access
    /// to the current user; child directories inherit that ACL. Unix storage
    /// is created with mode 0700 even when the parent is shared.
    pub fn capture_in(
        source: &Path,
        storage_parent: &Path,
        limits: SeedLimits,
    ) -> Result<Self, SeedError> {
        let canonical_source = source
            .canonicalize()
            .map_err(|error| io("resolve source", source, error))?;
        let canonical_parent = storage_parent
            .canonicalize()
            .map_err(|error| io("resolve storage parent", storage_parent, error))?;
        if canonical_parent.starts_with(&canonical_source) {
            return Err(SeedError::Unsupported {
                path: canonical_parent,
                reason: "private seed storage must be outside the source workspace",
            });
        }
        let original = open_dir(source)?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("harn-replay-");
        #[cfg(unix)]
        builder.permissions(
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        );
        let storage = storage::PrivateStorage(
            builder
                .tempdir_in(storage_parent)
                .map_err(|error| io("create private storage", storage_parent, error))?,
        );
        let parent = open_dir(storage.path())?;
        parent
            .create_dir("seed")
            .map_err(|error| io("create", Path::new("seed"), error))?;
        let seed = parent
            .open_dir("seed")
            .map_err(|error| io("open", Path::new("seed"), error))?;
        let receipt = copy_and_measure(&original, Some(&seed), limits)?;
        if copy_and_measure(&original, None, limits)? != receipt {
            return Err(SeedError::SourceChanged);
        }
        parent
            .create_dir("workspace")
            .map_err(|error| io("create", Path::new("workspace"), error))?;
        let workspace_dir = parent
            .open_dir("workspace")
            .map_err(|error| io("open", Path::new("workspace"), error))?;
        if copy_and_measure(&seed, Some(&workspace_dir), limits)? != receipt {
            return Err(SeedError::SeedChanged);
        }
        Ok(Self {
            workspace: storage.path().join("workspace"),
            storage,
            receipt,
            limits,
        })
    }

    /// Stable directory to grant to an isolated execution.
    pub fn path(&self) -> &Path {
        &self.workspace
    }

    /// Captured identity carried across every successful restore.
    pub fn receipt(&self) -> &SeedReceipt {
        &self.receipt
    }

    /// Replace only this object's private working copy, keeping its pathname
    /// stable. Verify the seed before retiring the previous copy.
    pub fn restore(&mut self) -> Result<&SeedReceipt, SeedError> {
        let parent = open_dir(self.storage.path())?;
        let seed = parent
            .open_dir("seed")
            .map_err(|error| io("open", Path::new("seed"), error))?;
        let staging = storage::PrivateStorage(
            tempfile::Builder::new()
                .prefix("restore-")
                .tempdir_in(self.storage.path())
                .map_err(|error| io("create staging", self.storage.path(), error))?,
        );
        let next = open_dir(staging.path())?;
        if copy_and_measure(&seed, Some(&next), self.limits)? != self.receipt {
            return Err(SeedError::SeedChanged);
        }
        drop(next);
        let staged_name = staging.path().file_name().expect("staging has a name");
        match parent.symlink_metadata("previous") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io("inspect previous copy", Path::new("previous"), error)),
            Ok(_) => {
                return Err(SeedError::Unsupported {
                    path: PathBuf::from("previous"),
                    reason: "a previous restore has unfinished cleanup",
                })
            }
        }
        parent
            .rename("workspace", &parent, "previous")
            .map_err(|error| io("retire working copy", &self.workspace, error))?;
        if let Err(error) = parent.rename(staged_name, &parent, "workspace") {
            parent
                .rename("previous", &parent, "workspace")
                .map_err(|rollback| {
                    io("recover previous working copy", &self.workspace, rollback)
                })?;
            return Err(io("publish restored copy", &self.workspace, error));
        }
        storage::make_child_removable(&parent, Path::new("previous")).map_err(|error| {
            io(
                "prepare retired copy for removal",
                Path::new("previous"),
                error,
            )
        })?;
        parent.remove_dir_all("previous").map_err(|error| {
            io(
                "remove retired copy after successful restore",
                Path::new("previous"),
                error,
            )
        })?;
        Ok(&self.receipt)
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Entry {
    Directory {
        mode: u32,
    },
    Link {
        target: String,
        directory: bool,
    },
    File {
        sha256: String,
        bytes: u64,
        mode: u32,
    },
}

fn open_dir(path: &Path) -> Result<Dir, SeedError> {
    Dir::open_ambient_dir(path, cap_std::ambient_authority())
        .map_err(|error| io("open directory", path, error))
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> SeedError {
    SeedError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn limit(resource: &'static str, observed: u64, maximum: u64) -> Result<(), SeedError> {
    if observed > maximum {
        Err(SeedError::Limit {
            resource,
            observed,
            limit: maximum,
        })
    } else {
        Ok(())
    }
}

fn copy_and_measure(
    source: &Dir,
    destination: Option<&Dir>,
    limits: SeedLimits,
) -> Result<SeedReceipt, SeedError> {
    let mut manifest = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    let mut files = 0;
    let mut bytes = 0u64;
    let root_permissions = source
        .dir_metadata()
        .map_err(|error| io("inspect root", Path::new("."), error))?
        .permissions();
    let root_mode = directory_permission_mode(&root_permissions);
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = source
            .read_dir(if directory.as_os_str().is_empty() {
                Path::new(".")
            } else {
                &directory
            })
            .map_err(|error| io("enumerate", &directory, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io("enumerate", &directory, error))?;
            let path = directory.join(entry.file_name());
            let name = path
                .iter()
                .map(|part| part.to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| SeedError::Unsupported {
                    path: path.clone(),
                    reason: "the path is not UTF-8",
                })?
                .join("/");
            limit(
                "entries",
                (manifest.len() + 1) as u64,
                limits.max_entries as u64,
            )?;
            limit(
                "path depth",
                path.components().count() as u64,
                limits.max_depth as u64,
            )?;
            let metadata = source
                .symlink_metadata(&path)
                .map_err(|error| io("inspect", &path, error))?;
            if metadata.is_dir() {
                if let Some(target) = destination {
                    target
                        .create_dir(&path)
                        .map_err(|error| io("create directory", &path, error))?;
                }
                let mode = directory_permission_mode(&metadata.permissions());
                directories.push((path.clone(), metadata.permissions()));
                pending.push(path);
                manifest.insert(name, Entry::Directory { mode });
            } else if metadata.is_file() {
                limit("file bytes", metadata.len(), limits.max_file_bytes)?;
                let mut file = source
                    .open(&path)
                    .map_err(|error| io("open file", &path, error))?;
                let opened = file
                    .metadata()
                    .map_err(|error| io("inspect open file", &path, error))?;
                if !opened.is_file() {
                    return Err(SeedError::Unsupported {
                        path,
                        reason: "the entry is not a regular file",
                    });
                }
                let remaining = limits.max_bytes.saturating_sub(bytes);
                let ceiling = limits.max_file_bytes.min(remaining);
                let mut content = Vec::new();
                (&mut file)
                    .take(ceiling.saturating_add(1))
                    .read_to_end(&mut content)
                    .map_err(|error| io("read file", &path, error))?;
                limit("file bytes", content.len() as u64, limits.max_file_bytes)?;
                bytes = bytes.saturating_add(content.len() as u64);
                limit("total bytes", bytes, limits.max_bytes)?;
                let permissions = opened.permissions();
                let mode = permission_mode(&permissions);
                #[cfg(unix)]
                let permissions = <Permissions as cap_std::fs::PermissionsExt>::from_mode(mode);
                if let Some(target) = destination {
                    let mut output = target
                        .create(&path)
                        .map_err(|error| io("create file", &path, error))?;
                    output
                        .write_all(&content)
                        .map_err(|error| io("write file", &path, error))?;
                    output
                        .set_permissions(permissions)
                        .map_err(|error| io("set file permissions", &path, error))?;
                }
                files += 1;
                manifest.insert(
                    name,
                    Entry::File {
                        sha256: hex::encode(Sha256::digest(&content)),
                        bytes: content.len() as u64,
                        mode,
                    },
                );
            } else if metadata.file_type().is_symlink() {
                let link = links::copy(source, destination, &path)?;
                let Entry::Link { target, .. } = &link else {
                    unreachable!("link capture returns a link")
                };
                limit("file bytes", target.len() as u64, limits.max_file_bytes)?;
                bytes = bytes.saturating_add(target.len() as u64);
                limit("total bytes", bytes, limits.max_bytes)?;
                manifest.insert(name, link);
            } else {
                return Err(SeedError::Unsupported {
                    path,
                    reason: "special files cannot be replayed",
                });
            }
        }
    }
    if let Some(target) = destination {
        // Populate children before reinstating read-only parent directories.
        directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (path, permissions) in directories {
            target
                .set_permissions(&path, permissions)
                .map_err(|error| io("set directory permissions", &path, error))?;
        }
        target
            .set_permissions(".", root_permissions)
            .map_err(|error| io("set root permissions", Path::new("."), error))?;
    }
    let identity =
        serde_json::to_vec(&(root_mode, &manifest)).expect("seed manifest is serializable");
    Ok(SeedReceipt {
        schema: "harn.workspace-seed.v1",
        sha256: hex::encode(Sha256::digest(&identity)),
        files,
        entries: manifest.len(),
        bytes,
    })
}

#[cfg(unix)]
fn permission_mode(permissions: &Permissions) -> u32 {
    cap_std::fs::PermissionsExt::mode(permissions) & 0o777
}

fn directory_permission_mode(permissions: &Permissions) -> u32 {
    #[cfg(unix)]
    {
        cap_std::fs::PermissionsExt::mode(permissions) & 0o7777
    }
    #[cfg(not(unix))]
    {
        permission_mode(permissions)
    }
}

#[cfg(not(unix))]
fn permission_mode(permissions: &Permissions) -> u32 {
    u32::from(permissions.readonly())
}

#[cfg(test)]
mod tests;
