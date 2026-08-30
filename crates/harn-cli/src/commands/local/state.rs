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
//!
//! The *selection* half is owned by [`harn_vm::local_selection`] and merely
//! re-exported here: it is a routing fact other surfaces (`harn chat`) must
//! read, so it cannot live behind this binary crate's `pub(crate)`. PID
//! records stay here because process lifecycle is genuinely the CLI's job.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use harn_vm::local_selection::{read_selection, write_selection, LocalSelection};

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
    harn_vm::local_selection::local_state_dir(base_dir)
}

pub(crate) fn ensure_state_dir(base_dir: &Path) -> Result<PathBuf, String> {
    harn_vm::local_selection::ensure_local_state_dir(base_dir)
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

#[cfg(any(unix, test))]
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

    // Selection round-trip coverage lives with the owner, in
    // `harn_vm::local_selection`.

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
