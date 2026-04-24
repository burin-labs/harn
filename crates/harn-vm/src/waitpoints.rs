#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::event_log::{
    sanitize_topic_component, AnyEventLog, EventLog, LogError, LogEvent, Topic,
};
#[cfg(test)]
use tokio::sync::oneshot;

pub const WAITPOINT_STATE_TOPIC_PREFIX: &str = "waitpoint.state.";
pub const WAITPOINT_WAITS_TOPIC: &str = "waitpoint.waits";

#[cfg(test)]
thread_local! {
    static TEST_WAIT_SIGNALS: RefCell<Vec<WaitpointTestSignal>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
struct WaitpointTestSignal {
    wait_id: String,
    kind: WaitpointTestSignalKind,
    tx: oneshot::Sender<()>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitpointTestSignalKind {
    Started,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitpointStatus {
    #[default]
    Open,
    Completed,
    Cancelled,
}

impl WaitpointStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitpointWaitStatus {
    Completed,
    Cancelled,
    TimedOut,
    Interrupted,
}

impl WaitpointWaitStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }

    fn event_kind(self) -> &'static str {
        match self {
            Self::Completed => "waitpoint_wait_completed",
            Self::Cancelled => "waitpoint_wait_cancelled",
            Self::TimedOut => "waitpoint_wait_timed_out",
            Self::Interrupted => "waitpoint_wait_interrupted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaitpointRecord {
    pub id: String,
    pub status: WaitpointStatus,
    pub created_at: String,
    pub created_by: Option<String>,
    pub completed_at: Option<String>,
    pub completed_by: Option<String>,
    pub cancelled_at: Option<String>,
    pub cancelled_by: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl WaitpointRecord {
    pub fn open(
        id: impl Into<String>,
        created_by: Option<String>,
        metadata: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            id: id.into(),
            status: WaitpointStatus::Open,
            created_at: now_rfc3339(),
            created_by,
            completed_at: None,
            completed_by: None,
            cancelled_at: None,
            cancelled_by: None,
            reason: None,
            metadata,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            WaitpointStatus::Completed | WaitpointStatus::Cancelled
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaitpointWaitStartRecord {
    pub wait_id: String,
    pub waitpoint_ids: Vec<String>,
    pub started_at: String,
    pub trace_id: Option<String>,
    pub replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaitpointWaitRecord {
    pub wait_id: String,
    pub waitpoint_ids: Vec<String>,
    pub status: WaitpointWaitStatus,
    pub started_at: String,
    pub resolved_at: String,
    pub waitpoints: Vec<WaitpointRecord>,
    pub cancelled_waitpoint_id: Option<String>,
    pub trace_id: Option<String>,
    pub replay_of_event_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitpointResolution {
    Pending,
    Completed,
    Cancelled { waitpoint_id: String },
}

pub fn dedupe_waitpoint_ids(ids: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in ids {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub fn waitpoint_topic(id: &str) -> Result<Topic, LogError> {
    Topic::new(format!(
        "{WAITPOINT_STATE_TOPIC_PREFIX}{}",
        sanitize_topic_component(id)
    ))
}

pub fn waits_topic() -> Result<Topic, LogError> {
    Topic::new(WAITPOINT_WAITS_TOPIC)
}

pub async fn load_waitpoint(
    log: &Arc<AnyEventLog>,
    id: &str,
) -> Result<Option<WaitpointRecord>, LogError> {
    let events = log
        .read_range(&waitpoint_topic(id)?, None, usize::MAX)
        .await?;
    let mut latest = None;
    for (_, event) in events {
        if !matches!(
            event.kind.as_str(),
            "waitpoint_created" | "waitpoint_completed" | "waitpoint_cancelled"
        ) {
            continue;
        }
        let Ok(record) = serde_json::from_value::<WaitpointRecord>(event.payload) else {
            continue;
        };
        latest = Some(record);
    }
    Ok(latest)
}

pub async fn load_waitpoints(
    log: &Arc<AnyEventLog>,
    ids: &[String],
) -> Result<Vec<WaitpointRecord>, LogError> {
    let mut out = Vec::new();
    for id in dedupe_waitpoint_ids(ids) {
        if let Some(record) = load_waitpoint(log, &id).await? {
            out.push(record);
        }
    }
    Ok(out)
}

pub fn resolve_waitpoints(ids: &[String], waitpoints: &[WaitpointRecord]) -> WaitpointResolution {
    let mut by_id = BTreeMap::new();
    for waitpoint in waitpoints {
        by_id.insert(waitpoint.id.as_str(), waitpoint);
    }
    let ids = dedupe_waitpoint_ids(ids);
    if ids.is_empty() {
        return WaitpointResolution::Pending;
    }

    let mut all_completed = true;
    for id in ids {
        let Some(waitpoint) = by_id.get(id.as_str()) else {
            all_completed = false;
            continue;
        };
        match waitpoint.status {
            WaitpointStatus::Completed => {}
            WaitpointStatus::Cancelled => {
                return WaitpointResolution::Cancelled {
                    waitpoint_id: waitpoint.id.clone(),
                };
            }
            WaitpointStatus::Open => {
                all_completed = false;
            }
        }
    }

    if all_completed {
        WaitpointResolution::Completed
    } else {
        WaitpointResolution::Pending
    }
}

pub async fn create_waitpoint(
    log: &Arc<AnyEventLog>,
    id: &str,
    created_by: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
) -> Result<WaitpointRecord, LogError> {
    if let Some(existing) = load_waitpoint(log, id).await? {
        return Ok(existing);
    }
    let record = WaitpointRecord::open(id, created_by, metadata);
    append_waitpoint_state(log, "waitpoint_created", &record).await?;
    Ok(record)
}

pub async fn complete_waitpoint(
    log: &Arc<AnyEventLog>,
    id: &str,
    completed_by: Option<String>,
) -> Result<WaitpointRecord, LogError> {
    let existing = load_waitpoint(log, id).await?;
    if let Some(existing) = existing.as_ref() {
        if existing.is_terminal() {
            return Ok(existing.clone());
        }
    }

    let now = now_rfc3339();
    let mut record = existing.unwrap_or_else(|| WaitpointRecord {
        id: id.to_string(),
        status: WaitpointStatus::Open,
        created_at: now.clone(),
        created_by: completed_by.clone(),
        completed_at: None,
        completed_by: None,
        cancelled_at: None,
        cancelled_by: None,
        reason: None,
        metadata: BTreeMap::new(),
    });
    record.status = WaitpointStatus::Completed;
    record.completed_at = Some(now);
    record.completed_by = completed_by;
    record.cancelled_at = None;
    record.cancelled_by = None;
    record.reason = None;
    append_waitpoint_state(log, "waitpoint_completed", &record).await?;
    Ok(record)
}

pub async fn cancel_waitpoint(
    log: &Arc<AnyEventLog>,
    id: &str,
    cancelled_by: Option<String>,
    reason: Option<String>,
) -> Result<WaitpointRecord, LogError> {
    let existing = load_waitpoint(log, id).await?;
    if let Some(existing) = existing.as_ref() {
        if existing.is_terminal() {
            return Ok(existing.clone());
        }
    }

    let now = now_rfc3339();
    let mut record = existing.unwrap_or_else(|| WaitpointRecord {
        id: id.to_string(),
        status: WaitpointStatus::Open,
        created_at: now.clone(),
        created_by: cancelled_by.clone(),
        completed_at: None,
        completed_by: None,
        cancelled_at: None,
        cancelled_by: None,
        reason: None,
        metadata: BTreeMap::new(),
    });
    record.status = WaitpointStatus::Cancelled;
    record.completed_at = None;
    record.completed_by = None;
    record.cancelled_at = Some(now);
    record.cancelled_by = cancelled_by;
    record.reason = reason;
    append_waitpoint_state(log, "waitpoint_cancelled", &record).await?;
    Ok(record)
}

pub async fn append_wait_started(
    log: &Arc<AnyEventLog>,
    record: &WaitpointWaitStartRecord,
) -> Result<(), LogError> {
    log.append(
        &waits_topic()?,
        LogEvent::new(
            "waitpoint_wait_started",
            serde_json::to_value(record).map_err(|error| {
                LogError::Serde(format!("waitpoint wait encode error: {error}"))
            })?,
        )
        .with_headers(wait_headers(&record.wait_id, &record.waitpoint_ids)),
    )
    .await
    .map(|_| ())?;
    notify_test_wait_started(&record.wait_id);
    Ok(())
}

pub async fn append_wait_terminal(
    log: &Arc<AnyEventLog>,
    record: &WaitpointWaitRecord,
) -> Result<(), LogError> {
    log.append(
        &waits_topic()?,
        LogEvent::new(
            record.status.event_kind(),
            serde_json::to_value(record).map_err(|error| {
                LogError::Serde(format!("waitpoint wait encode error: {error}"))
            })?,
        )
        .with_headers(wait_headers(&record.wait_id, &record.waitpoint_ids)),
    )
    .await
    .map(|_| ())?;
    if record.status == WaitpointWaitStatus::Interrupted {
        notify_test_wait_interrupted(&record.wait_id);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn install_test_wait_signal(
    wait_id: impl Into<String>,
    kind: WaitpointTestSignalKind,
    tx: oneshot::Sender<()>,
) {
    TEST_WAIT_SIGNALS.with(|slot| {
        slot.borrow_mut().push(WaitpointTestSignal {
            wait_id: wait_id.into(),
            kind,
            tx,
        });
    });
}

#[cfg(test)]
pub(crate) fn clear_test_wait_signals() {
    TEST_WAIT_SIGNALS.with(|slot| slot.borrow_mut().clear());
}

#[cfg(not(test))]
fn notify_test_wait_started(_wait_id: &str) {}

#[cfg(test)]
fn notify_test_wait_started(wait_id: &str) {
    notify_test_wait_signal(wait_id, WaitpointTestSignalKind::Started);
}

#[cfg(not(test))]
fn notify_test_wait_interrupted(_wait_id: &str) {}

#[cfg(test)]
fn notify_test_wait_interrupted(wait_id: &str) {
    notify_test_wait_signal(wait_id, WaitpointTestSignalKind::Interrupted);
}

#[cfg(test)]
fn notify_test_wait_signal(wait_id: &str, kind: WaitpointTestSignalKind) {
    TEST_WAIT_SIGNALS.with(|slot| {
        let mut signals = slot.borrow_mut();
        let mut index = 0;
        while index < signals.len() {
            if signals[index].wait_id == wait_id && signals[index].kind == kind {
                let signal = signals.remove(index);
                let _ = signal.tx.send(());
            } else {
                index += 1;
            }
        }
    });
}

pub async fn find_wait_terminal(
    log: &Arc<AnyEventLog>,
    wait_id: &str,
) -> Result<Option<WaitpointWaitRecord>, LogError> {
    let events = log.read_range(&waits_topic()?, None, usize::MAX).await?;
    let mut latest = None;
    for (_, event) in events {
        if !matches!(
            event.kind.as_str(),
            "waitpoint_wait_completed"
                | "waitpoint_wait_cancelled"
                | "waitpoint_wait_timed_out"
                | "waitpoint_wait_interrupted"
        ) {
            continue;
        }
        if event.headers.get("wait_id").map(String::as_str) != Some(wait_id) {
            continue;
        }
        let Ok(record) = serde_json::from_value::<WaitpointWaitRecord>(event.payload) else {
            continue;
        };
        latest = Some(record);
    }
    Ok(latest)
}

async fn append_waitpoint_state(
    log: &Arc<AnyEventLog>,
    kind: &str,
    record: &WaitpointRecord,
) -> Result<(), LogError> {
    log.append(
        &waitpoint_topic(&record.id)?,
        LogEvent::new(
            kind,
            serde_json::to_value(record)
                .map_err(|error| LogError::Serde(format!("waitpoint encode error: {error}")))?,
        )
        .with_headers(waitpoint_headers(record)),
    )
    .await
    .map(|_| ())
}

fn wait_headers(wait_id: &str, waitpoint_ids: &[String]) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("wait_id".to_string(), wait_id.to_string());
    headers.insert("waitpoints".to_string(), waitpoint_ids.join(","));
    headers
}

fn waitpoint_headers(record: &WaitpointRecord) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("waitpoint_id".to_string(), record.id.clone());
    headers.insert("status".to_string(), record.status.as_str().to_string());
    if let Some(created_by) = record.created_by.as_ref() {
        headers.insert("created_by".to_string(), created_by.clone());
    }
    if let Some(completed_by) = record.completed_by.as_ref() {
        headers.insert("completed_by".to_string(), completed_by.clone());
    }
    if let Some(cancelled_by) = record.cancelled_by.as_ref() {
        headers.insert("cancelled_by".to_string(), cancelled_by.clone());
    }
    headers
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{FileEventLog, MemoryEventLog};

    #[tokio::test]
    async fn waitpoint_state_persists_across_file_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = Arc::new(AnyEventLog::File(
            FileEventLog::open(dir.path().to_path_buf(), 32).expect("open file log"),
        ));
        create_waitpoint(&first, "demo", Some("creator".to_string()), BTreeMap::new())
            .await
            .expect("create waitpoint");
        complete_waitpoint(&first, "demo", Some("completer".to_string()))
            .await
            .expect("complete waitpoint");

        let reopened = Arc::new(AnyEventLog::File(
            FileEventLog::open(dir.path().to_path_buf(), 32).expect("reopen file log"),
        ));
        let state = load_waitpoint(&reopened, "demo")
            .await
            .expect("load state")
            .expect("waitpoint exists");
        assert_eq!(state.status, WaitpointStatus::Completed);
        assert_eq!(state.completed_by.as_deref(), Some("completer"));
    }

    #[tokio::test]
    async fn wait_terminal_lookup_returns_latest_terminal_record() {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        append_wait_started(
            &log,
            &WaitpointWaitStartRecord {
                wait_id: "wait-demo".to_string(),
                waitpoint_ids: vec!["a".to_string(), "b".to_string()],
                started_at: "2026-01-01T00:00:00Z".to_string(),
                trace_id: Some("trace-demo".to_string()),
                replay_of_event_id: None,
            },
        )
        .await
        .expect("append wait start");
        append_wait_terminal(
            &log,
            &WaitpointWaitRecord {
                wait_id: "wait-demo".to_string(),
                waitpoint_ids: vec!["a".to_string(), "b".to_string()],
                status: WaitpointWaitStatus::TimedOut,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                resolved_at: "2026-01-01T00:01:00Z".to_string(),
                waitpoints: Vec::new(),
                cancelled_waitpoint_id: None,
                trace_id: Some("trace-demo".to_string()),
                replay_of_event_id: None,
                reason: Some("deadline elapsed".to_string()),
            },
        )
        .await
        .expect("append wait result");

        let record = find_wait_terminal(&log, "wait-demo")
            .await
            .expect("lookup wait result")
            .expect("wait result exists");
        assert_eq!(record.status, WaitpointWaitStatus::TimedOut);
        assert_eq!(record.reason.as_deref(), Some("deadline elapsed"));
    }
}
