use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::value::{VmError, VmValue};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct DaemonLoopConfig {
    pub persist_path: Option<String>,
    pub resume_path: Option<String>,
    pub wake_interval_ms: Option<u64>,
    pub watch_paths: Vec<String>,
    pub consolidate_on_idle: bool,
    /// Maximum number of consecutive idle-wait attempts that can return
    /// `None` (no wake reason) before the daemon watchdog trips. A bridge
    /// that never signals, an empty watch-path set, and no wake_interval
    /// would otherwise leave the daemon blocked forever. `None` disables
    /// the watchdog; `Some(0)` trips on the first idle attempt.
    pub idle_watchdog_attempts: Option<usize>,
}

impl DaemonLoopConfig {
    pub(crate) fn effective_persist_path(&self) -> Option<&str> {
        self.persist_path.as_deref().or(self.resume_path.as_deref())
    }

    pub(crate) fn has_wake_source(&self, has_bridge: bool) -> bool {
        has_bridge || self.wake_interval_ms.is_some() || !self.watch_paths.is_empty()
    }

    pub(crate) fn idle_wait_ms(&self, idle_backoff_ms: u64) -> u64 {
        self.wake_interval_ms.unwrap_or(idle_backoff_ms.max(1))
    }

    pub(crate) fn update_idle_backoff(&self, idle_backoff_ms: &mut u64) {
        if let Some(fixed) = self.wake_interval_ms {
            *idle_backoff_ms = fixed.max(1);
            return;
        }
        *idle_backoff_ms = match *idle_backoff_ms {
            0..=100 => 500,
            101..=500 => 1000,
            1001..=1999 => 2000,
            _ => 2000,
        };
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct DaemonSnapshot {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub saved_at: String,
    pub daemon_state: String,
    pub visible_messages: Vec<serde_json::Value>,
    pub recorded_messages: Vec<serde_json::Value>,
    pub transcript_summary: Option<String>,
    pub transcript_events: Vec<serde_json::Value>,
    pub total_text: String,
    pub last_iteration_text: String,
    pub all_tools_used: Vec<String>,
    pub rejected_tools: Vec<String>,
    pub deferred_user_messages: Vec<String>,
    pub total_iterations: usize,
    pub idle_backoff_ms: u64,
    pub last_run_exit_code: Option<i32>,
    pub watch_state: BTreeMap<String, u64>,
}

impl DaemonSnapshot {
    pub(crate) fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = "daemon_snapshot".to_string();
        }
        if self.saved_at.is_empty() {
            self.saved_at = crate::orchestration::now_rfc3339();
        }
        if self.daemon_state.is_empty() {
            self.daemon_state = "active".to_string();
        }
        self
    }
}

/// Snapshot a file's mtime as nanoseconds since the Unix epoch.
/// Nanosecond precision avoids a second-boundary race: two edits to
/// the same path less than a full second apart would both read the
/// same `as_secs()` value and be reported as unchanged, which has
/// bitten the watch test on coarser-resolution filesystems. u64
/// covers nanos-since-epoch through year 2554.
fn file_stamp(path: &str) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

pub(crate) fn watch_state(paths: &[String]) -> BTreeMap<String, u64> {
    paths
        .iter()
        .map(|path| (path.clone(), file_stamp(path)))
        .collect::<BTreeMap<_, _>>()
}

pub(crate) fn detect_watch_changes(
    paths: &[String],
    previous: &mut BTreeMap<String, u64>,
) -> Vec<String> {
    let mut changed = Vec::new();
    for path in paths {
        let current = file_stamp(path);
        let prior = previous.get(path).copied().unwrap_or(0);
        if prior != 0 && current != 0 && current != prior {
            changed.push(path.clone());
        }
        previous.insert(path.clone(), current);
    }
    changed
}

pub(crate) fn persist_snapshot(path: &str, snapshot: &DaemonSnapshot) -> Result<String, VmError> {
    let path_buf = Path::new(path);
    if let Some(parent) = path_buf.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| VmError::Runtime(format!("daemon snapshot mkdir error: {error}")))?;
    }
    let json = serde_json::to_string_pretty(&snapshot.clone().normalize())
        .map_err(|error| VmError::Runtime(format!("daemon snapshot encode error: {error}")))?;
    let tmp = path_buf.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .map_err(|error| VmError::Runtime(format!("daemon snapshot write error: {error}")))?;
    std::fs::rename(&tmp, path_buf)
        .map_err(|error| VmError::Runtime(format!("daemon snapshot finalize error: {error}")))?;
    Ok(path.to_string())
}

pub(crate) fn load_snapshot(path: &str) -> Result<DaemonSnapshot, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| VmError::Runtime(format!("daemon snapshot read error: {error}")))?;
    let snapshot: DaemonSnapshot = serde_json::from_str(&content)
        .map_err(|error| VmError::Runtime(format!("daemon snapshot parse error: {error}")))?;
    Ok(snapshot.normalize())
}

pub(crate) fn parse_daemon_loop_config(
    options: Option<&BTreeMap<String, VmValue>>,
) -> DaemonLoopConfig {
    let Some(options) = options else {
        return DaemonLoopConfig::default();
    };

    let watch_paths = match options.get("watch_paths") {
        Some(VmValue::List(items)) => items
            .iter()
            .map(VmValue::display)
            .filter(|path| !path.is_empty())
            .collect(),
        Some(VmValue::String(path)) if !path.is_empty() => vec![path.to_string()],
        Some(value) => {
            let path = value.display();
            if path.is_empty() {
                Vec::new()
            } else {
                vec![path]
            }
        }
        None => Vec::new(),
    };

    DaemonLoopConfig {
        persist_path: options
            .get("persist_path")
            .map(VmValue::display)
            .filter(|value| !value.is_empty()),
        resume_path: options
            .get("resume_path")
            .map(VmValue::display)
            .filter(|value| !value.is_empty()),
        wake_interval_ms: options
            .get("wake_interval_ms")
            .and_then(|value| value.as_int())
            .map(|value| value as u64)
            .filter(|value| *value > 0),
        watch_paths,
        consolidate_on_idle: options
            .get("consolidate_on_idle")
            .is_some_and(|value| matches!(value, VmValue::Bool(true))),
        idle_watchdog_attempts: options
            .get("idle_watchdog_attempts")
            .and_then(|value| value.as_int())
            .and_then(|value| usize::try_from(value).ok()),
    }
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
