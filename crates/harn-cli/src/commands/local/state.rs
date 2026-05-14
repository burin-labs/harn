//! Persistent state for `harn local`:
//!
//! - the currently-selected local provider/model, written by `harn local
//!   switch` and surfaced by `harn local status`;
//! - PID files for self-launched llama.cpp / MLX processes, written when
//!   `harn local switch` launches a server itself and consumed by
//!   `harn local stop` / `harn local list`.
//!
//! Lives under `<state_root>/local/` (where `<state_root>` defaults to
//! `<cwd>/.harn` and honors `HARN_STATE_DIR`). Treats missing files as the
//! "no prior selection / no Harn-managed process" state so first-run flows
//! work without an explicit init.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const LOCAL_SUBDIR: &str = "local";
const SELECTION_FILE: &str = "selection.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalSelection {
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
    pub(crate) fn now(
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
            switched_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| String::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PidRecord {
    pub provider: String,
    pub pid: u32,
    pub model: String,
    pub base_url: String,
    pub command: String,
    pub args: Vec<String>,
    pub started_at: String,
}

pub(crate) fn local_state_dir(base_dir: &Path) -> PathBuf {
    harn_vm::runtime_paths::state_root(base_dir).join(LOCAL_SUBDIR)
}

pub(crate) fn ensure_state_dir(base_dir: &Path) -> Result<PathBuf, String> {
    let dir = local_state_dir(base_dir);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

pub(crate) fn write_selection(base_dir: &Path, selection: &LocalSelection) -> Result<(), String> {
    let dir = ensure_state_dir(base_dir)?;
    let path = dir.join(SELECTION_FILE);
    let body = serde_json::to_vec_pretty(selection)
        .map_err(|error| format!("failed to serialize local selection: {error}"))?;
    fs::write(&path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

pub(crate) fn read_selection(base_dir: &Path) -> Result<Option<LocalSelection>, String> {
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

fn pid_file(base_dir: &Path, provider: &str) -> PathBuf {
    local_state_dir(base_dir).join(format!("{provider}.pid.json"))
}

/// Reserved for the upcoming `harn local switch --launch` path that will
/// spawn llama.cpp / MLX servers itself. Currently only the test suite
/// exercises this, but it ships alongside the read/clear helpers so the
/// state-dir contract stays one file from the moment `harn local launch`
/// lands.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_pid_record(base_dir: &Path, record: &PidRecord) -> Result<(), String> {
    let dir = ensure_state_dir(base_dir)?;
    let path = dir.join(format!("{}.pid.json", record.provider));
    let body = serde_json::to_vec_pretty(record)
        .map_err(|error| format!("failed to serialize pid record: {error}"))?;
    fs::write(&path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

pub(crate) fn read_pid_record(
    base_dir: &Path,
    provider: &str,
) -> Result<Option<PidRecord>, String> {
    let path = pid_file(base_dir, provider);
    if !path.exists() {
        return Ok(None);
    }
    let body =
        fs::read(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let record: PidRecord = serde_json::from_slice(&body)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    Ok(Some(record))
}

pub(crate) fn clear_pid_record(base_dir: &Path, provider: &str) -> Result<(), String> {
    let path = pid_file(base_dir, provider);
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(&path).map_err(|error| format!("failed to remove {}: {error}", path.display()))
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

    #[test]
    fn pid_record_roundtrip_and_clear() {
        let dir = tempdir().expect("tempdir");
        let record = PidRecord {
            provider: "llamacpp".to_string(),
            pid: 4242,
            model: "Qwen3.6-Coder-30B".to_string(),
            base_url: "http://127.0.0.1:8001".to_string(),
            command: "llama-server".to_string(),
            args: vec!["--port".to_string(), "8001".to_string()],
            started_at: "2026-05-14T00:00:00Z".to_string(),
        };
        write_pid_record(dir.path(), &record).expect("write");
        let round = read_pid_record(dir.path(), "llamacpp")
            .expect("read")
            .expect("present");
        assert_eq!(round, record);
        clear_pid_record(dir.path(), "llamacpp").expect("clear");
        assert!(read_pid_record(dir.path(), "llamacpp")
            .expect("read after clear")
            .is_none());
    }
}
