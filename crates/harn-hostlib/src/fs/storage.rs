//! The staged filesystem's existing manifest, audit attempts, and content bodies.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    atomic_write, batch, default_root, manifest_path, normalize_logical, now_ms, session_dir,
    FsMode, Manifest, SessionState, StagedEntry, MANIFEST_VERSION,
};

pub(super) fn load_state(session_id: &str, root: Option<PathBuf>) -> Result<SessionState, String> {
    let root = root.unwrap_or_else(default_root);
    let manifest_path = manifest_path(&root, session_id);
    if manifest_path.exists() {
        let text = fs::read_to_string(&manifest_path)
            .map_err(|err| format!("read {}: {err}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|err| format!("parse {}: {err}", manifest_path.display()))?;
        if manifest.version != MANIFEST_VERSION {
            return Err(format!(
                "unsupported staged fs manifest version {} in {}",
                manifest.version,
                manifest_path.display()
            ));
        }
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
        for group in manifest.atomic_groups.values() {
            group.validate(&manifest_root)?;
        }
        return Ok(SessionState {
            session_id: manifest.session_id,
            mode: manifest.mode,
            root: manifest_root,
            atomic_groups: manifest.atomic_groups,
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
    let dir = session_dir(&state.root, &state.session_id);
    fs::create_dir_all(dir.join("bodies"))
        .map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        session_id: state.session_id.clone(),
        mode: state.mode,
        root: state.root.to_string_lossy().into_owned(),
        atomic_groups: state.atomic_groups.clone(),
        entries: state
            .entries
            .iter()
            .map(|(path, entry)| (path.to_string_lossy().into_owned(), entry.clone()))
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("serialize staged manifest: {err}"))?;
    append_journal(state, op, path)?;
    atomic_write(&manifest_path(&state.root, &state.session_id), &bytes)?;
    prune_unreferenced_bodies(state);
    Ok(())
}

fn append_journal(state: &SessionState, op: &str, path: Option<&Path>) -> Result<(), String> {
    let dir = session_dir(&state.root, &state.session_id);
    fs::create_dir_all(&dir).map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
    let line = serde_json::to_string(&serde_json::json!({
        "ts_ms": now_ms(),
        "op": op,
        "phase": "attempt",
        "path": path.map(|path| path.to_string_lossy().into_owned()), // agent-path-ok: on-disk journal.jsonl audit line, never returned to the agent
        "pending_count": state.entries.len(),
    }))
    .map_err(|err| format!("serialize staged journal: {err}"))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("journal.jsonl"))
        .map_err(|err| format!("open staged journal: {err}"))?;
    writeln!(file, "{line}").map_err(|err| format!("write staged journal: {err}"))
}

pub(super) fn write_body(state: &SessionState, bytes: &[u8]) -> Result<String, String> {
    let hash = hex::encode(Sha256::digest(bytes));
    let path = session_dir(&state.root, &state.session_id)
        .join("bodies")
        .join(&hash);
    if !path.exists() {
        atomic_write(&path, bytes)?;
    }
    Ok(hash)
}

pub(super) fn read_body(state: &SessionState, hash: &str) -> std::io::Result<Vec<u8>> {
    if !batch::valid_body_hash(hash) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid staged body hash",
        ));
    }
    let bytes = fs::read(
        session_dir(&state.root, &state.session_id)
            .join("bodies")
            .join(hash),
    )?;
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
    let body_dir = session_dir(&state.root, &state.session_id).join("bodies");
    let Ok(entries) = fs::read_dir(&body_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !live.contains(&name) {
            let _ = fs::remove_file(entry.path());
        }
    }
}
