use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::{effects, Group, Image, Images};
use crate::fs::{
    check_fs_path_scope, commit_entry, emit_staged_update, mutation_state, persist_state,
    selected_paths, to_agent_path, write_body, CommitResult, DiscardResult, FsAccess, FsMode,
    HostlibError, SessionState, StagedEntry, COMMIT_BUILTIN, DISCARD_BUILTIN,
};

fn selected(state: &SessionState, paths: &[String]) -> BTreeSet<PathBuf> {
    let mut selected: BTreeSet<_> = selected_paths(state, paths).into_iter().collect();
    for group in state.atomic_groups.values() {
        if group.original().keys().any(|path| selected.contains(path)) {
            selected.extend(group.original().keys().cloned());
        }
    }
    selected
}

fn capture(state: &SessionState, paths: &BTreeSet<PathBuf>) -> Result<Images, String> {
    let anchor = crate::fs::anchored_file::Root::open(&state.root)?;
    paths
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&state.root)
                .map_err(|_| "file group path is outside its root")?
                .to_str()
                .ok_or("file group path is not UTF-8")?;
            let image = anchor
                .parent_of(relative, false)?
                .map(|parent| parent.image())
                .transpose()?
                .flatten();
            let image = image
                .map(|image| {
                    Ok::<Image, String>(Image {
                        body_hash: write_body(state, &image.bytes)?,
                        permissions: u64::from(image.permissions),
                    })
                })
                .transpose()?;
            Ok((path.clone(), image))
        })
        .collect()
}

fn lock_paths(state: &SessionState, selected: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let mut paths = selected.clone();
    for group in state
        .atomic_groups
        .values()
        .filter(|group| group.needs_recovery())
    {
        paths.extend(group.original().keys().cloned());
    }
    paths
}

fn recover(state: &mut SessionState) -> Result<bool, String> {
    let pending: Vec<_> = state
        .atomic_groups
        .iter()
        .filter(|(_, group)| group.needs_recovery())
        .map(|(id, _)| id.clone())
        .collect();
    for id in &pending {
        for (path, original) in state.atomic_groups[id].original() {
            let access = if original.is_some() {
                FsAccess::Write
            } else {
                FsAccess::Delete
            };
            check_fs_path_scope(path, access)
                .map_err(|violation| violation.message(COMMIT_BUILTIN))?;
        }
        effects::recover(state, id)?;
    }
    Ok(!pending.is_empty())
}

pub(in crate::fs) fn commit(
    session_id: &str,
    paths: &[String],
) -> Result<CommitResult, HostlibError> {
    let reject = |message: String| HostlibError::Backend {
        builtin: COMMIT_BUILTIN,
        message,
    };
    let snapshot = {
        let (_cache, state, _manifest_locks) = mutation_state(session_id, None, COMMIT_BUILTIN)?;
        state
    };
    let selected = selected(&snapshot, paths);
    let locked: Vec<_> = lock_paths(&snapshot, &selected).into_iter().collect();
    let _file_locks = harn_vm::conditional_replace::acquire_locks(&locked)
        .map_err(|error| reject(error.to_string()))?;
    if _file_locks.len() != locked.len() {
        return Err(reject("staged paths alias the same destination".into()));
    }
    let (mut cache, mut state, _manifest_locks) = mutation_state(session_id, None, COMMIT_BUILTIN)?;
    if state.root != snapshot.root
        || state.entries != snapshot.entries
        || state.atomic_groups != snapshot.atomic_groups
    {
        return Err(reject(
            "staged data changed while acquiring file locks; retry".into(),
        ));
    }
    let result = (|| {
        if recover(&mut state)? {
            return Err(
                "recovered an incomplete file transaction; review staged data again".into(),
            );
        }
        let mut regular = BTreeSet::new();
        let mut directories = Vec::new();
        for path in &selected {
            let entry = state
                .entries
                .get(path)
                .ok_or("file group has an incomplete staged census")?;
            let access = match entry {
                StagedEntry::Write { .. } => FsAccess::Write,
                StagedEntry::Delete { .. } => FsAccess::Delete,
            };
            check_fs_path_scope(path, access)
                .map_err(|violation| violation.message(COMMIT_BUILTIN))?;
            if matches!(entry, StagedEntry::Delete { .. })
                && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
            {
                if state
                    .atomic_groups
                    .values()
                    .any(|group| group.original().contains_key(path))
                {
                    return Err("a regular-file group path became a directory".into());
                }
                directories.push(path.clone());
            } else {
                regular.insert(path.clone());
            }
        }
        let mut committed = Vec::new();
        let mut failed = Vec::new();
        if !regular.is_empty() {
            let mut original = capture(&state, &regular)?;
            let mut permissions: BTreeMap<_, _> = original
                .iter()
                .map(|(path, image)| (path.clone(), image.as_ref().map(|image| image.permissions)))
                .collect();
            let ids: Vec<_> = state
                .atomic_groups
                .iter()
                .filter(|(_, group)| group.original().keys().any(|path| regular.contains(path)))
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                let Group::Staged {
                    original: before,
                    permissions: modes,
                } = &state.atomic_groups[id]
                else {
                    return Err("file group still requires recovery".into());
                };
                if !before.keys().all(|path| regular.contains(path)) {
                    return Err("file group selection is incomplete".into());
                }
                original.extend(before.clone());
                permissions.extend(modes.clone());
            }
            let before = state.clone();
            for id in ids {
                state.atomic_groups.remove(&id);
            }
            let id = format!("file-group-{}", uuid::Uuid::new_v4());
            state.atomic_groups.insert(
                id.clone(),
                Group::Staged {
                    original,
                    permissions,
                },
            );
            match effects::apply(&mut state, &id, FsMode::Staged) {
                Ok(()) => committed.extend(regular.iter().map(to_agent_path)),
                Err(reason) => {
                    if !state
                        .atomic_groups
                        .get(&id)
                        .is_some_and(Group::needs_recovery)
                    {
                        state = before;
                        persist_state(&state, "retain_staged_file_group", None)?;
                    }
                    failed.extend(
                        regular
                            .iter()
                            .map(|path| (to_agent_path(path), reason.clone())),
                    );
                }
            }
        }
        // Directory deletion retains its existing independent operation contract.
        for path in directories {
            match commit_entry(&state, &path, &state.entries[&path]) {
                Ok(()) => {
                    state.entries.remove(&path);
                    committed.push(to_agent_path(&path));
                }
                Err(reason) => failed.push((to_agent_path(&path), reason)),
            }
        }
        persist_state(&state, "commit_staged", None)?;
        Ok(CommitResult {
            committed_paths: committed,
            failed_paths_with_reasons: failed,
        })
    })();
    emit_staged_update(&state);
    cache.insert(session_id.to_string(), state);
    result.map_err(reject)
}

pub(in crate::fs) fn discard(
    session_id: &str,
    paths: &[String],
) -> Result<DiscardResult, HostlibError> {
    let reject = |message: String| HostlibError::Backend {
        builtin: DISCARD_BUILTIN,
        message,
    };
    let snapshot = {
        let (_cache, state, _manifest_locks) = mutation_state(session_id, None, DISCARD_BUILTIN)?;
        state
    };
    let selected = selected(&snapshot, paths);
    let locked: Vec<_> = lock_paths(&snapshot, &selected).into_iter().collect();
    let _file_locks = harn_vm::conditional_replace::acquire_locks(&locked)
        .map_err(|error| reject(error.to_string()))?;
    let (mut cache, mut state, _manifest_locks) =
        mutation_state(session_id, None, DISCARD_BUILTIN)?;
    if state.root != snapshot.root
        || state.entries != snapshot.entries
        || state.atomic_groups != snapshot.atomic_groups
    {
        return Err(reject(
            "staged data changed while acquiring file locks; retry".into(),
        ));
    }
    let result = (|| {
        recover(&mut state)?;
        let mut next = state.clone();
        let discarded_paths = selected
            .iter()
            .filter_map(|path| next.entries.remove(path).map(|_| to_agent_path(path)))
            .collect();
        next.atomic_groups
            .retain(|_, group| !group.original().keys().any(|path| selected.contains(path)));
        persist_state(&next, "discard_staged", None)?;
        state = next;
        Ok(DiscardResult { discarded_paths })
    })();
    emit_staged_update(&state);
    cache.insert(session_id.to_string(), state);
    result.map_err(reject)
}
