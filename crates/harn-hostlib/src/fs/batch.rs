//! Regular-file groups in the existing staged filesystem manifest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::paths::normalize_against;
use super::FsMode;

/// A preconditioned regular-file mutation in a refactoring transaction.
pub(crate) struct FileChange {
    pub path: PathBuf,
    pub expected_hash: Option<String>,
    pub contents: Option<Vec<u8>>,
    pub permissions_from: Option<PathBuf>,
}

#[cfg(unix)]
mod effects;
#[cfg(unix)]
pub(super) mod staged;
#[cfg(all(test, unix))]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    pub body_hash: String,
    pub permissions: u64,
}

pub(super) type Images = BTreeMap<PathBuf, Option<Image>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Group {
    Staged {
        original: Images,
        permissions: BTreeMap<PathBuf, Option<u64>>,
    },
    Committing {
        original: Images,
        desired: Images,
        origin_mode: FsMode,
    },
}

impl Group {
    pub(super) fn original(&self) -> &Images {
        match self {
            Self::Staged { original, .. } | Self::Committing { original, .. } => original,
        }
    }

    pub(super) fn needs_recovery(&self) -> bool {
        matches!(self, Self::Committing { .. })
    }

    pub(super) fn body_hashes(&self) -> impl Iterator<Item = &str> {
        let desired = match self {
            Self::Staged { .. } => None,
            Self::Committing { desired, .. } => Some(desired),
        };
        self.original()
            .values()
            .chain(desired.into_iter().flat_map(BTreeMap::values))
            .filter_map(Option::as_ref)
            .map(|image| image.body_hash.as_str())
    }

    pub(super) fn validate(&self, root: &Path) -> Result<(), String> {
        let original = self.original();
        if original.is_empty() {
            return Err("an atomic file group has no recorded paths".into());
        }
        validate_images(root, original)?;
        match self {
            Self::Staged { permissions, .. } => {
                for (path, mode) in permissions {
                    if !original.contains_key(path) || mode.is_some_and(|mode| mode > 0o7777) {
                        return Err("an atomic file group has invalid permissions".into());
                    }
                }
            }
            Self::Committing { desired, .. } => {
                validate_images(root, desired)?;
                if !original.keys().eq(desired.keys()) {
                    return Err("an atomic file group has an incomplete desired census".into());
                }
            }
        }
        Ok(())
    }
}

fn validate_images(root: &Path, images: &Images) -> Result<(), String> {
    for (path, image) in images {
        if !path.is_absolute()
            || path == root
            || !path.starts_with(root)
            || normalize_against(root, path) != *path
        {
            return Err("an atomic file group contains a path outside its root".into());
        }
        if let Some(image) = image {
            if image.permissions > 0o7777 || !valid_body_hash(&image.body_hash) {
                return Err("an atomic file group contains an invalid image".into());
            }
        }
    }
    Ok(())
}

pub(super) fn valid_body_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(unix)]
fn input_images(
    state: &super::SessionState,
    root: &Path,
    changes: &[FileChange],
    retain_bodies: bool,
) -> Result<Images, String> {
    use super::anchored_file::Root;
    use sha2::{Digest, Sha256};
    let anchor = Root::open(root)?;
    let mut original = BTreeMap::new();
    for change in changes {
        let relative = change
            .path
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .ok_or("invalid file transaction path")?;
        let physical = anchor
            .parent_of(relative, false)?
            .map(|parent| parent.image())
            .transpose()?
            .flatten();
        let visible = if state.mode == FsMode::Staged {
            match super::overlay_read(state, &change.path) {
                Some(Ok(bytes)) => Some(bytes),
                Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
                Some(Err(error)) => return Err(error.to_string()),
                None => physical.as_ref().map(|image| image.bytes.clone()),
            }
        } else {
            if state.entries.contains_key(&change.path) {
                return Err(
                    "discard or commit pending staged data before an immediate file transaction"
                        .into(),
                );
            }
            physical.as_ref().map(|image| image.bytes.clone())
        };
        let observed_hash = visible
            .as_ref()
            .map(|bytes| hex::encode(Sha256::digest(bytes)));
        if observed_hash != change.expected_hash {
            return Err("file transaction input changed since it was planned".into());
        }
        let image = physical
            .map(|image| {
                let hash = if retain_bodies {
                    super::write_body(state, &image.bytes)?
                } else {
                    hex::encode(Sha256::digest(&image.bytes))
                };
                Ok::<_, String>(Image {
                    body_hash: hash,
                    permissions: u64::from(image.permissions),
                })
            })
            .transpose()?;
        original.insert(change.path.clone(), image);
    }
    Ok(original)
}

#[cfg(unix)]
pub(crate) fn apply_changes(
    builtin: &'static str,
    root: &Path,
    changes: &[FileChange],
    session: Option<&str>,
    before_effects: impl FnOnce(FsMode) -> Result<(), crate::error::HostlibError>,
) -> Result<(), crate::error::HostlibError> {
    use super::paths::{active_session_id, normalize_logical};
    use super::{locked_state, persist_state, write_body, StagedEntry};
    use harn_vm::process_sandbox::{check_fs_path_scope, FsAccess};
    use std::collections::BTreeSet;

    let reject = |message: String| crate::error::HostlibError::Backend { builtin, message };
    if changes.is_empty() {
        return Ok(());
    }
    let root = normalize_logical(root);
    let session = active_session_id(session).unwrap_or_else(|| "file-refactor".into());
    let snapshot = {
        let (_cache, state, _manifest_locks) = locked_state(&session, Some(root.clone()), builtin)?;
        state
    };
    if snapshot.root != root {
        return Err(reject(
            "file transaction root differs from its session root".into(),
        ));
    }
    let requested: BTreeSet<_> = changes.iter().map(|change| change.path.clone()).collect();
    if requested.len() != changes.len() {
        return Err(reject("file transaction repeats a destination".into()));
    }
    for change in changes {
        if change.path == root
            || normalize_against(&root, &change.path) != change.path
            || !change.path.starts_with(&root)
        {
            return Err(reject("file transaction path is outside its root".into()));
        }
        let access = if change.contents.is_some() {
            FsAccess::Write
        } else {
            FsAccess::Delete
        };
        check_fs_path_scope(&change.path, access)
            .map_err(|violation| reject(violation.message(builtin)))?;
        if change
            .expected_hash
            .as_ref()
            .is_some_and(|hash| !valid_body_hash(hash))
        {
            return Err(reject("file transaction has an invalid input hash".into()));
        }
    }
    let mut locked_paths = requested.clone();
    for group in snapshot
        .atomic_groups
        .values()
        .filter(|group| group.needs_recovery())
    {
        locked_paths.extend(group.original().keys().cloned());
        for (path, original) in group.original() {
            let access = if original.is_some() {
                FsAccess::Write
            } else {
                FsAccess::Delete
            };
            check_fs_path_scope(path, access)
                .map_err(|violation| reject(violation.message(builtin)))?;
        }
    }
    let paths: Vec<_> = locked_paths.into_iter().collect();
    let _file_locks = harn_vm::conditional_replace::acquire_locks(&paths)
        .map_err(|error| reject(error.to_string()))?;
    if _file_locks.len() != paths.len() {
        return Err(reject(
            "file transaction paths alias the same destination".into(),
        ));
    }
    {
        let (mut cache, mut state, _manifest_locks) =
            locked_state(&session, Some(root.clone()), builtin)?;
        let pending: Vec<_> = state
            .atomic_groups
            .iter()
            .filter(|(_, group)| group.needs_recovery())
            .map(|(id, _)| id.clone())
            .collect();
        let recovered = (|| {
            for id in &pending {
                let group = &state.atomic_groups[id];
                if !group.original().keys().all(|path| paths.contains(path)) {
                    return Err(
                        "file recovery changed while acquiring its paths; retry".to_string()
                    );
                }
                effects::recover(&mut state, id)?;
            }
            Ok(())
        })();
        cache.insert(session.clone(), state);
        recovered.map_err(reject)?;
        if !pending.is_empty() {
            return Err(reject(
                "recovered an incomplete file transaction; rebuild the refactoring plan".into(),
            ));
        }
    }
    input_images(&snapshot, &root, changes, false).map_err(reject)?;
    before_effects(snapshot.mode)?;
    let (mut cache, state, _manifest_locks) = locked_state(&session, Some(root.clone()), builtin)?;
    if state.mode != snapshot.mode || state.root != root {
        return Err(reject(
            "file transaction session changed while preparing; retry".into(),
        ));
    }
    if state
        .atomic_groups
        .values()
        .any(|group| group.original().keys().any(|path| requested.contains(path)))
    {
        return Err(reject(
            "commit or discard the existing file group before changing its paths".into(),
        ));
    }
    let original = input_images(&state, &root, changes, true).map_err(reject)?;
    let mut permissions = BTreeMap::new();
    for change in changes {
        if let Some(source) = &change.permissions_from {
            if !original.contains_key(source) {
                return Err(reject(
                    "file transaction has an unknown permission source".into(),
                ));
            }
        }
        if change.contents.is_some() {
            let source = change.permissions_from.as_ref().unwrap_or(&change.path);
            permissions.insert(
                change.path.clone(),
                original[source].as_ref().map(|image| image.permissions),
            );
        }
    }
    let mut next = state.clone();
    for change in changes {
        let entry = match &change.contents {
            Some(bytes) => StagedEntry::Write {
                body_hash: write_body(&next, bytes).map_err(reject)?,
                len: bytes.len() as u64,
                created_at_ms: super::now_ms(),
                snapshot_id: harn_vm::agent_sessions::current_tool_call_id(),
            },
            None => StagedEntry::Delete {
                recursive: false,
                created_at_ms: super::now_ms(),
                snapshot_id: harn_vm::agent_sessions::current_tool_call_id(),
            },
        };
        next.entries.insert(change.path.clone(), entry);
    }
    let id = format!("file-group-{}", uuid::Uuid::new_v4());
    next.atomic_groups.insert(
        id.clone(),
        Group::Staged {
            original,
            permissions,
        },
    );
    let outcome = if next.mode == FsMode::Staged {
        persist_state(&next, "stage_file_group", None)
    } else {
        effects::apply(&mut next, &id, FsMode::Immediate)
    };
    if outcome.is_ok()
        || next
            .atomic_groups
            .get(&id)
            .is_some_and(Group::needs_recovery)
    {
        cache.insert(session, next);
    } else {
        cache.insert(session, state);
    }
    outcome.map_err(reject)
}

#[cfg(not(unix))]
pub(crate) fn apply_changes(
    builtin: &'static str,
    _root: &Path,
    _changes: &[FileChange],
    _session: Option<&str>,
    _before_effects: impl FnOnce(FsMode) -> Result<(), crate::error::HostlibError>,
) -> Result<(), crate::error::HostlibError> {
    Err(crate::error::HostlibError::Backend {
        builtin,
        message: "atomic file transactions are unavailable on this host".into(),
    })
}
