use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{Group, Image, Images};
use crate::fs::anchored_file::{Parent, PreparedWrite, Root};
use crate::fs::storage::{persist_state, read_body};
use crate::fs::{FsMode, SessionState, StagedEntry};

#[cfg(test)]
thread_local! {
    pub(super) static EXIT_AFTER_FIRST_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn parents(
    root: &Path,
    images: &Images,
    create: bool,
) -> Result<BTreeMap<PathBuf, Parent>, String> {
    let anchor = Root::open(root)?;
    for (path, expected) in images {
        if expected.is_some() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "file group path is outside its root")?
                .to_str()
                .ok_or("file group path is not UTF-8")?;
            anchor
                .parent_of(relative, false)?
                .ok_or("file group parent directory is missing")?;
        }
    }
    images
        .keys()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "file group path is outside its root")?
                .to_str()
                .ok_or("file group path is not UTF-8")?;
            let parent = anchor
                .parent_of(relative, create)?
                .ok_or("file group parent directory is missing")?;
            Ok((path.clone(), parent))
        })
        .collect()
}

fn image(parent: &Parent) -> Result<Option<Image>, String> {
    parent.image().map(|image| {
        image.map(|image| Image {
            body_hash: hex::encode(Sha256::digest(&image.bytes)),
            permissions: u64::from(image.permissions),
        })
    })
}

fn actual(parents: &BTreeMap<PathBuf, Parent>) -> Result<Images, String> {
    parents
        .iter()
        .map(|(path, parent)| Ok((path.clone(), image(parent)?)))
        .collect()
}

fn prepare_undo<'a>(
    state: &SessionState,
    parents: &'a BTreeMap<PathBuf, Parent>,
    original: &Images,
    observed: &Images,
) -> Result<BTreeMap<PathBuf, PreparedWrite<'a>>, String> {
    let mut undo = BTreeMap::new();
    for (path, before) in original {
        if observed.get(path) == Some(before) {
            continue;
        }
        if let Some(before) = before {
            let bytes = read_body(state, &before.body_hash).map_err(|error| error.to_string())?;
            let mode = libc::mode_t::try_from(before.permissions)
                .map_err(|_| "invalid original permissions")?;
            undo.insert(
                path.clone(),
                parents[path].prepare(&bytes, false, Some(mode))?,
            );
        }
    }
    Ok(undo)
}

fn restore(
    parents: &BTreeMap<PathBuf, Parent>,
    original: &Images,
    desired: &Images,
    undo: &mut BTreeMap<PathBuf, PreparedWrite<'_>>,
) -> Result<(), String> {
    let observed = actual(parents)?;
    for (path, value) in &observed {
        if original.get(path) != Some(value) && desired.get(path) != Some(value) {
            return Err(
                "file group recovery found an unattributed change; retained its journal".into(),
            );
        }
    }
    for (path, before) in original {
        if observed.get(path) == Some(before) {
            continue;
        }
        let parent = &parents[path];
        let current = image(parent)?;
        if Some(&current) == original.get(path) {
            continue;
        }
        if Some(&current) != desired.get(path) {
            return Err("file group changed during recovery; retained its journal".into());
        }
        if before.is_some() {
            let prepared = undo
                .remove(path)
                .ok_or("missing prepared original file image")?;
            if !prepared.publish(current.is_none())? {
                return Err("file appeared during group recovery; retained its journal".into());
            }
        } else {
            parent.unlink()?;
        }
    }
    if actual(parents)? != *original {
        return Err("file group recovery did not restore every original image".into());
    }
    Ok(())
}

fn record_rollback(
    state: &mut SessionState,
    id: &str,
    original: Images,
    desired: &Images,
    origin_mode: FsMode,
) -> Result<(), String> {
    let mut recovered = state.clone();
    if origin_mode == FsMode::Staged {
        let permissions = desired
            .iter()
            .filter_map(|(path, image)| {
                image
                    .as_ref()
                    .map(|image| (path.clone(), Some(image.permissions)))
            })
            .collect();
        recovered.atomic_groups.insert(
            id.to_string(),
            Group::Staged {
                original,
                permissions,
            },
        );
    } else {
        for path in original.keys() {
            recovered.entries.remove(path);
        }
        recovered.atomic_groups.remove(id);
    }
    persist_state(&recovered, "rollback_file_group", None)?;
    *state = recovered;
    Ok(())
}

pub(super) fn recover(state: &mut SessionState, id: &str) -> Result<(), String> {
    let Some(Group::Committing {
        original,
        desired,
        origin_mode,
    }) = state.atomic_groups.get(id).cloned()
    else {
        return Ok(());
    };
    let parents = parents(&state.root, &original, false)?;
    let observed = actual(&parents)?;
    for (path, current) in &observed {
        if original.get(path) != Some(current) && desired.get(path) != Some(current) {
            return Err(
                "file group recovery found an unattributed change; retained its journal".into(),
            );
        }
    }
    let mut undo = prepare_undo(state, &parents, &original, &observed)?;
    restore(&parents, &original, &desired, &mut undo)?;
    record_rollback(state, id, original, &desired, origin_mode)
}

pub(super) fn apply(state: &mut SessionState, id: &str, origin_mode: FsMode) -> Result<(), String> {
    let Some(Group::Staged {
        original,
        permissions,
    }) = state.atomic_groups.get(id).cloned()
    else {
        return Err("file group is missing or requires recovery".into());
    };
    let parents = parents(&state.root, &original, true)?;
    if actual(&parents)? != original {
        return Err("file group input changed since it was staged".into());
    }
    let mut writes = BTreeMap::new();
    let mut desired = BTreeMap::new();
    for path in original.keys() {
        let after = match state
            .entries
            .get(path)
            .ok_or("file group has an incomplete staged census")?
        {
            StagedEntry::Write { body_hash, len, .. } => {
                let bytes = read_body(state, body_hash).map_err(|error| error.to_string())?;
                if u64::try_from(bytes.len()).ok() != Some(*len) {
                    return Err("staged file body differs from its recorded length".into());
                }
                let mode = permissions
                    .get(path)
                    .copied()
                    .flatten()
                    .map(libc::mode_t::try_from)
                    .transpose()
                    .map_err(|_| "invalid desired permissions")?;
                let prepared = parents[path].prepare(&bytes, false, mode)?;
                let after = Some(Image {
                    body_hash: body_hash.clone(),
                    permissions: u64::from(prepared.permissions()),
                });
                writes.insert(path.clone(), prepared);
                after
            }
            StagedEntry::Delete { .. } => None,
        };
        desired.insert(path.clone(), after);
    }
    // Prepare original files before effects too, so a later publish failure
    // does not depend on allocating another temporary file to restore them.
    let mut undo = prepare_undo(state, &parents, &original, &desired)?;
    if actual(&parents)? != original {
        return Err("file group input changed while preparing its images".into());
    }
    state.atomic_groups.insert(
        id.to_string(),
        Group::Committing {
            original: original.clone(),
            desired: desired.clone(),
            origin_mode,
        },
    );
    if let Err(error) = persist_state(state, "prepare_file_group", None) {
        state.atomic_groups.insert(
            id.to_string(),
            Group::Staged {
                original,
                permissions,
            },
        );
        return Err(error);
    }
    let applied = (|| {
        for path in original.keys() {
            let parent = &parents[path];
            if image(parent)? != original[path] {
                return Err("file group input changed before publication".to_string());
            }
            if let Some(prepared) = writes.remove(path) {
                if !prepared.publish(original[path].is_none())? {
                    return Err("file appeared before group publication".into());
                }
            } else if original[path].is_some() {
                parent.unlink()?;
            }
            #[cfg(test)]
            if EXIT_AFTER_FIRST_PUBLISH.with(|armed| armed.replace(false)) {
                std::process::exit(73);
            }
        }
        if actual(&parents)? != desired {
            return Err("file group publication did not reach every desired image".into());
        }
        let mut completed = state.clone();
        for path in original.keys() {
            completed.entries.remove(path);
        }
        completed.atomic_groups.remove(id);
        persist_state(&completed, "commit_file_group", None)?;
        *state = completed;
        Ok(())
    })();
    match applied {
        Ok(()) => Ok(()),
        Err(reason) => {
            restore(&parents, &original, &desired, &mut undo)
                .and_then(|()| record_rollback(state, id, original, &desired, origin_mode))
                .map_err(|rollback| format!("{reason}; recovery remains pending: {rollback}"))?;
            Err(reason)
        }
    }
}
