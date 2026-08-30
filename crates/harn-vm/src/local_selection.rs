//! The active local-model selection: which local provider and model the user
//! last pointed Harn at with `harn local switch`.
//!
//! This lives in the runtime rather than in the CLI because it is a *routing*
//! fact, not a CLI presentation detail. `harn local status` renders it and
//! `harn chat` routes to it; both read this one owner instead of each
//! re-deriving the file layout. It sits next to [`crate::runtime_paths`],
//! which already owns the `<state_root>` path this file is anchored to.
//!
//! Stored at `<state_root>/local/selection.json`, where `<state_root>`
//! defaults to `<cwd>/.harn` and honors `HARN_STATE_DIR`. A missing file is
//! the "no prior selection" state, not an error, so first-run flows work
//! without an explicit init.
//!
//! Reading this is deliberately *not* the same as changing global provider
//! defaults: a surface that wants to honor the selection asks for it, so a
//! stale switch can never silently re-route a command that never opted in.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const LOCAL_SUBDIR: &str = "local";
const SELECTION_FILE: &str = "selection.json";

/// The local provider/model pair `harn local switch` last selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSelection {
    pub provider: String,
    pub model: String,
    pub alias: Option<String>,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
    pub switched_at: String,
}

impl LocalSelection {
    pub fn now(
        provider: impl Into<String>,
        model: impl Into<String>,
        alias: Option<String>,
        base_url: impl Into<String>,
        ctx: Option<u64>,
        keep_alive: Option<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            alias,
            base_url: base_url.into(),
            ctx,
            keep_alive,
            switched_at: harn_clock::system_now_rfc3339(),
        }
    }
}

/// The directory holding local-runtime state for `base_dir`.
pub fn local_state_dir(base_dir: &Path) -> PathBuf {
    crate::runtime_paths::state_root(base_dir).join(LOCAL_SUBDIR)
}

pub fn ensure_local_state_dir(base_dir: &Path) -> Result<PathBuf, String> {
    let dir = local_state_dir(base_dir);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

pub fn write_selection(base_dir: &Path, selection: &LocalSelection) -> Result<(), String> {
    let dir = ensure_local_state_dir(base_dir)?;
    let path = dir.join(SELECTION_FILE);
    let body = serde_json::to_vec_pretty(selection)
        .map_err(|error| format!("failed to serialize local selection: {error}"))?;
    fs::write(&path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

/// Read the active selection, or `None` when the user has never switched.
pub fn read_selection(base_dir: &Path) -> Result<Option<LocalSelection>, String> {
    let path = local_state_dir(base_dir).join(SELECTION_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let body =
        fs::read(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let selection: LocalSelection = serde_json::from_slice(&body)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    Ok(Some(selection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn selection_roundtrip_persists_under_state_root() {
        let dir = tempdir().expect("tempdir");
        let selection = LocalSelection::now(
            "ollama",
            "qwen36:30b",
            Some("qwen36-coder".to_string()),
            "http://127.0.0.1:11434",
            Some(32_768),
            Some("30m".to_string()),
        );
        write_selection(dir.path(), &selection).expect("write selection");
        let round = read_selection(dir.path())
            .expect("read selection")
            .expect("present");
        assert_eq!(round.provider, "ollama");
        assert_eq!(round.model, "qwen36:30b");
        assert_eq!(round.alias.as_deref(), Some("qwen36-coder"));
        assert_eq!(round.ctx, Some(32_768));
    }

    #[test]
    fn read_selection_returns_none_when_missing() {
        let dir = tempdir().expect("tempdir");
        let result = read_selection(dir.path()).expect("ok");
        assert!(result.is_none());
    }
}
