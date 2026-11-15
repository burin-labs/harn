//! The staged filesystem's existing manifest, audit attempts, and content bodies.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    batch, default_root, manifest_path, normalize_logical, now_ms, session_dir, FsMode, Manifest,
    SessionState, StagedEntry, MANIFEST_VERSION,
};

mod directory;
use directory::SessionStorage;

pub(super) fn load_state(session_id: &str, root: Option<PathBuf>) -> Result<SessionState, String> {
    let root = root.unwrap_or_else(default_root);
    let manifest_path = manifest_path(&root, session_id);
    let storage = SessionStorage::open(&root, session_id)
        .map_err(|error| format!("open staged workspace {}: {error}", root.display()))?;
    let text = match storage.read(Path::new("manifest.json")) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("read {}: {error}", manifest_path.display())),
    };
    if let Some(text) = text {
        let manifest: Manifest = serde_json::from_slice(&text)
            .map_err(|err| format!("parse {}: {err}", manifest_path.display()))?;
        let atomic_groups = match manifest.version {
            1 if manifest.atomic_groups.is_none() => BTreeMap::new(),
            MANIFEST_VERSION => manifest
                .atomic_groups
                .ok_or("staged fs manifest is missing its mandatory file-group census")?,
            version => {
                return Err(format!(
                    "unsupported staged fs manifest version {version} in {}",
                    manifest_path.display()
                ))
            }
        };
        if manifest.session_id != session_id {
            return Err(format!(
                "staged fs manifest session id mismatch in {}",
                manifest_path.display()
            ));
        }
        let manifest_root = normalize_logical(Path::new(&manifest.root));
        if manifest_root != root {
            return Err("staged fs manifest root does not match its configured root".into());
        }
        for group in atomic_groups.values() {
            group.validate(&manifest_root)?;
        }
        return Ok(SessionState {
            session_id: manifest.session_id,
            mode: manifest.mode,
            root: manifest_root,
            atomic_groups,
            entries: manifest
                .entries
                .into_iter()
                .map(|(path, entry)| (normalize_logical(Path::new(&path)), entry))
                .collect(),
        });
    }
    Ok(SessionState {
        session_id: session_id.to_string(),
        mode: FsMode::Immediate,
        root,
        entries: BTreeMap::new(),
        atomic_groups: BTreeMap::new(),
    })
}

pub(super) fn persist_state(
    state: &SessionState,
    op: &str,
    path: Option<&Path>,
) -> Result<(), String> {
    let storage = SessionStorage::open(&state.root, &state.session_id)
        .map_err(|error| format!("open staged workspace: {error}"))?;
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        session_id: state.session_id.clone(),
        mode: state.mode,
        root: state.root.to_string_lossy().into_owned(),
        atomic_groups: Some(state.atomic_groups.clone()),
        entries: state
            .entries
            .iter()
            .map(|(path, entry)| (path.to_string_lossy().into_owned(), entry.clone()))
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("serialize staged manifest: {err}"))?;
    append_journal(state, op, path)?;
    storage
        .write(Path::new("manifest.json"), &bytes)
        .map_err(|error| format!("write staged manifest: {error}"))?;
    prune_unreferenced_bodies(state);
    Ok(())
}

fn append_journal(state: &SessionState, op: &str, path: Option<&Path>) -> Result<(), String> {
    let line = serde_json::to_string(&serde_json::json!({
        "ts_ms": now_ms(),
        "op": op,
        "phase": "attempt",
        "path": path.map(|path| path.to_string_lossy().into_owned()), // agent-path-ok: on-disk journal.jsonl audit line, never returned to the agent
        "pending_count": state.entries.len(),
    }))
    .map_err(|err| format!("serialize staged journal: {err}"))?;
    SessionStorage::open(&state.root, &state.session_id)
        .and_then(|storage| {
            storage.append(Path::new("journal.jsonl"), format!("{line}\n").as_bytes())
        })
        .map_err(|error| format!("append staged journal: {error}"))
}

pub(super) fn write_body(state: &SessionState, bytes: &[u8]) -> Result<String, String> {
    let hash = hex::encode(Sha256::digest(bytes));
    SessionStorage::open(&state.root, &state.session_id)
        .and_then(|storage| storage.write(&Path::new("bodies").join(&hash), bytes))
        .map_err(|error| format!("write staged body: {error}"))?;
    Ok(hash)
}

pub(super) fn read_body(state: &SessionState, hash: &str) -> std::io::Result<Vec<u8>> {
    if !batch::valid_body_hash(hash) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid staged body hash",
        ));
    }
    let bytes = SessionStorage::open(&state.root, &state.session_id)?
        .read(&Path::new("bodies").join(hash))?;
    if hex::encode(Sha256::digest(&bytes)) != hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "staged body does not match its hash",
        ));
    }
    Ok(bytes)
}

fn prune_unreferenced_bodies(state: &SessionState) {
    let mut live: BTreeSet<String> = state
        .entries
        .values()
        .filter_map(|entry| match entry {
            StagedEntry::Write { body_hash, .. } => Some(body_hash.clone()),
            StagedEntry::Delete { .. } => None,
        })
        .collect();
    live.extend(
        state
            .atomic_groups
            .values()
            .flat_map(batch::Group::body_hashes)
            .map(str::to_string),
    );
    let _ = SessionStorage::open(&state.root, &state.session_id)
        .and_then(|storage| storage.prune_bodies(&live));
}

pub(super) fn remove_state(state: &SessionState) -> std::io::Result<()> {
    SessionStorage::open(&state.root, &state.session_id)?.remove()
}

#[cfg(test)]
mod tests;
