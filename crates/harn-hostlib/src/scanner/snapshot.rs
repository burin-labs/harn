//! Snapshot persistence for incremental scans.
//!
//! Snapshots live under `<root>/.harn/hostlib/scanner-snapshot.json`, the
//! canonical per-repo Harn working directory. The snapshot stores both the
//! [`ScanResult`] and the canonicalized root so [`load_for_token`] can
//! refuse cross-root token reuse.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scanner::result::ScanResult;

const SNAPSHOT_REL_PATH: &str = ".harn/hostlib/scanner-snapshot.json";

/// Wrapper persisted to disk.
#[derive(Serialize, Deserialize)]
struct StoredSnapshot {
    schema_version: u32,
    root: String,
    result: ScanResult,
}

const STORED_SNAPSHOT_VERSION: u32 = 1;

/// Compute the snapshot path for a given canonicalized root.
pub fn snapshot_path(root: &Path) -> PathBuf {
    root.join(SNAPSHOT_REL_PATH)
}

/// Persist a [`ScanResult`] to disk under `<root>/.harn/hostlib/`. Best
/// effort — IO failures are swallowed because callers always have the
/// in-memory result to return.
pub fn save(root: &Path, result: &ScanResult) {
    let path = snapshot_path(root);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let stored = StoredSnapshot {
        schema_version: STORED_SNAPSHOT_VERSION,
        root: root.to_string_lossy().into_owned(),
        result: result.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&stored) {
        let _ = fs::write(&path, bytes);
    }
}

/// Load the snapshot at `root` if any.
pub fn load(root: &Path) -> Option<ScanResult> {
    let path = snapshot_path(root);
    let bytes = fs::read(&path).ok()?;
    let stored: StoredSnapshot = serde_json::from_slice(&bytes).ok()?;
    if stored.schema_version != STORED_SNAPSHOT_VERSION {
        return None;
    }
    Some(stored.result)
}

/// Convert a token (canonicalized root path) back to a `Path`.
pub fn token_to_root(token: &str) -> PathBuf {
    PathBuf::from(token)
}

/// Build a snapshot token for a path. Today this is just the canonicalized
/// path — opaque to consumers, but human-readable for debugging.
pub fn root_to_token(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::result::{ProjectMetadata, ScanResult};
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn empty_result(token: &str, root: &str) -> ScanResult {
        ScanResult {
            snapshot_token: token.to_string(),
            truncated: false,
            project: ProjectMetadata {
                name: "x".to_string(),
                root_path: root.to_string(),
                languages: Vec::new(),
                test_commands: BTreeMap::new(),
                detected_test_command: None,
                code_patterns: Vec::new(),
                total_files: 0,
                total_lines: 0,
                last_scanned_at: "1970-01-01T00:00:00Z".to_string(),
            },
            folders: Vec::new(),
            files: Vec::new(),
            symbols: Vec::new(),
            dependencies: Vec::new(),
            sub_projects: Vec::new(),
            repo_map: String::new(),
        }
    }

    #[test]
    fn round_trip_persists_and_reloads() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let token = root_to_token(root);
        let original = empty_result(&token, &token);
        save(root, &original);
        let loaded = load(root).expect("snapshot must round-trip");
        assert_eq!(loaded.snapshot_token, original.snapshot_token);
    }
}
