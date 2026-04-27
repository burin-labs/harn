use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::dto::PortalLaunchJob;
use super::launch::create_trigger_replay_job;
use super::state::PortalState;

const MAX_BULK_ITEMS: usize = 50;
const DEFAULT_BULK_RATE_LIMIT_PER_SECOND: f64 = 2.0;
const MAX_BULK_RATE_LIMIT_PER_SECOND: f64 = 10.0;

#[derive(Debug, Default, Deserialize)]
pub(super) struct DlqQuery {
    pub(super) trigger_id: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) error_class: Option<String>,
    pub(super) since: Option<String>,
    pub(super) until: Option<String>,
    pub(super) state: Option<String>,
    pub(super) q: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DlqBulkRequest {
    pub(super) trigger_id: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) error_class: Option<String>,
    pub(super) since: Option<String>,
    pub(super) until: Option<String>,
    pub(super) older_than_seconds: Option<i64>,
    pub(super) dry_run: Option<bool>,
    pub(super) rate_limit_per_second: Option<f64>,
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct PortalDlqAttempt {
    pub(super) attempt: u32,
    pub(super) at: String,
    pub(super) status: String,
    pub(super) error: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct PortalDlqEntry {
    pub(super) id: String,
    pub(super) event_id: String,
    pub(super) trigger_id: String,
    pub(super) binding_id: String,
    pub(super) binding_key: String,
    pub(super) binding_version: Option<u32>,
    pub(super) provider: String,
    pub(super) event_kind: String,
    pub(super) failed_at: String,
    pub(super) failed_at_ms: i64,
    pub(super) last_error: String,
    pub(super) error_class: String,
    pub(super) retry_count: u32,
    pub(super) state: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) payload: JsonValue,
    pub(super) event: JsonValue,
    pub(super) attempt_history: Vec<PortalDlqAttempt>,
    pub(super) predicate_trace: Vec<JsonValue>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalDlqGroup {
    pub(super) error_class: String,
    pub(super) count: usize,
    pub(super) newest_failed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalDlqAlert {
    pub(super) trigger_id: String,
    pub(super) error_class: String,
    pub(super) count: usize,
    pub(super) window_seconds: i64,
    pub(super) threshold_entries: u32,
    pub(super) destinations: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalDlqAlertConfig {
    pub(super) trigger_id: String,
    pub(super) destinations: Vec<String>,
    pub(super) threshold_entries: Option<u32>,
    pub(super) threshold_percent: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalDlqListResponse {
    pub(super) total: usize,
    pub(super) entries: Vec<PortalDlqEntry>,
    pub(super) groups: Vec<PortalDlqGroup>,
    pub(super) alerts: Vec<PortalDlqAlert>,
    pub(super) alert_configs: Vec<PortalDlqAlertConfig>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalDlqBulkResponse {
    pub(super) operation: String,
    pub(super) dry_run: bool,
    pub(super) matched_count: usize,
    pub(super) accepted_count: usize,
    pub(super) skipped_count: usize,
    pub(super) rate_limit_per_second: f64,
    pub(super) jobs: Vec<PortalLaunchJob>,
    pub(super) entries: Vec<PortalDlqEntry>,
}

pub(super) async fn list_dlq_entries(
    state: &Arc<PortalState>,
    query: &DlqQuery,
) -> Result<PortalDlqListResponse, String> {
    let event_log = state
        .event_log
        .as_ref()
        .ok_or_else(|| "portal is not attached to an event log".to_string())?;
    let mut entries = load_normalized_dlq(event_log).await?;
    attach_predicate_trace(event_log, &mut entries).await?;
    let entries = filter_entries(entries, query)?;
    let groups = group_by_error_class(&entries);
    let alert_configs = load_alert_configs(&state.workspace_root);
    let alerts = active_alerts(&entries, &alert_configs);
    Ok(PortalDlqListResponse {
        total: entries.len(),
        entries,
        groups,
        alerts,
        alert_configs,
    })
}

pub(super) async fn dlq_detail(
    state: &Arc<PortalState>,
    entry_id: &str,
) -> Result<PortalDlqEntry, String> {
    let query = DlqQuery {
        state: Some("all".to_string()),
        ..DlqQuery::default()
    };
    let mut response = list_dlq_entries(state, &query).await?;
    let entry = response
        .entries
        .drain(..)
        .find(|entry| entry.id == entry_id)
        .ok_or_else(|| format!("unknown DLQ entry '{entry_id}'"))?;
    Ok(entry)
}

pub(super) async fn replay_entry(
    state: &Arc<PortalState>,
    entry_id: &str,
    drift_accept: bool,
) -> Result<PortalLaunchJob, String> {
    let entry = dlq_detail(state, entry_id).await?;
    create_trigger_replay_job_with_mode(state, &entry, drift_accept).await
}

pub(super) async fn purge_entry(
    state: &Arc<PortalState>,
    entry_id: &str,
) -> Result<PortalDlqEntry, String> {
    let mut entry = dlq_detail(state, entry_id).await?;
    entry.state = "discarded".to_string();
    entry.attempt_history.push(PortalDlqAttempt {
        attempt: entry.attempt_history.len() as u32 + 1,
        at: now_rfc3339(),
        status: "discarded".to_string(),
        error: None,
    });
    append_portal_entry(state, &entry, "dlq_entry")
        .await
        .map_err(|error| format!("failed to purge DLQ entry: {error}"))?;
    Ok(entry)
}

pub(super) async fn export_entry(
    state: &Arc<PortalState>,
    entry_id: &str,
) -> Result<JsonValue, String> {
    let entry = dlq_detail(state, entry_id).await?;
    serde_json::to_value(serde_json::json!({
        "fixture_schema": "harn.dlq.fixture.v1",
        "entry": entry,
    }))
    .map_err(|error| error.to_string())
}

pub(super) async fn bulk_replay(
    state: &Arc<PortalState>,
    request: &DlqBulkRequest,
) -> Result<PortalDlqBulkResponse, String> {
    let entries = select_bulk_entries(state, request).await?;
    let dry_run = request.dry_run.unwrap_or(false);
    let rate_limit_per_second = sanitize_rate_limit(request.rate_limit_per_second);
    if dry_run {
        return Ok(PortalDlqBulkResponse {
            operation: "replay".to_string(),
            dry_run,
            matched_count: entries.len(),
            accepted_count: 0,
            skipped_count: entries.len(),
            rate_limit_per_second,
            jobs: Vec::new(),
            entries,
        });
    }

    let mut jobs = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        jobs.push(create_trigger_replay_job_with_mode(state, entry, false).await?);
        if index + 1 < entries.len() {
            tokio::time::sleep(Duration::from_secs_f64(1.0 / rate_limit_per_second)).await;
        }
    }
    Ok(PortalDlqBulkResponse {
        operation: "replay".to_string(),
        dry_run,
        matched_count: entries.len(),
        accepted_count: jobs.len(),
        skipped_count: 0,
        rate_limit_per_second,
        jobs,
        entries,
    })
}

pub(super) async fn bulk_purge(
    state: &Arc<PortalState>,
    request: &DlqBulkRequest,
) -> Result<PortalDlqBulkResponse, String> {
    let entries = select_bulk_entries(state, request).await?;
    let dry_run = request.dry_run.unwrap_or(false);
    let rate_limit_per_second = sanitize_rate_limit(request.rate_limit_per_second);
    let mut purged = Vec::new();
    if !dry_run {
        let total = entries.len();
        for (index, mut entry) in entries.clone().into_iter().enumerate() {
            entry.state = "discarded".to_string();
            entry.attempt_history.push(PortalDlqAttempt {
                attempt: entry.attempt_history.len() as u32 + 1,
                at: now_rfc3339(),
                status: "discarded".to_string(),
                error: None,
            });
            append_portal_entry(state, &entry, "dlq_entry").await?;
            purged.push(entry);
            if index + 1 < total {
                tokio::time::sleep(Duration::from_secs_f64(1.0 / rate_limit_per_second)).await;
            }
        }
    }
    Ok(PortalDlqBulkResponse {
        operation: "purge".to_string(),
        dry_run,
        matched_count: entries.len(),
        accepted_count: if dry_run { 0 } else { purged.len() },
        skipped_count: if dry_run { entries.len() } else { 0 },
        rate_limit_per_second,
        jobs: Vec::new(),
        entries: if dry_run { entries } else { purged },
    })
}

async fn select_bulk_entries(
    state: &Arc<PortalState>,
    request: &DlqBulkRequest,
) -> Result<Vec<PortalDlqEntry>, String> {
    let mut query = DlqQuery {
        trigger_id: request.trigger_id.clone(),
        provider: request.provider.clone(),
        error_class: request.error_class.clone(),
        since: request.since.clone(),
        until: request.until.clone(),
        state: Some("pending".to_string()),
        q: None,
    };
    if let Some(seconds) = request.older_than_seconds {
        let until = OffsetDateTime::now_utc()
            .saturating_sub(time::Duration::seconds(seconds.max(0)))
            .format(&Rfc3339)
            .unwrap_or_default();
        query.until = Some(until);
    }
    let response = list_dlq_entries(state, &query).await?;
    if response.entries.len() > MAX_BULK_ITEMS {
        return Err(format!(
            "bulk operation matched {} entries; narrow the filter below the {} entry safety limit",
            response.entries.len(),
            MAX_BULK_ITEMS
        ));
    }
    Ok(response.entries)
}

async fn create_trigger_replay_job_with_mode(
    state: &Arc<PortalState>,
    entry: &PortalDlqEntry,
    drift_accept: bool,
) -> Result<PortalLaunchJob, String> {
    let mut job = create_trigger_replay_job(state, &entry.event_id)
        .await
        .map_err(|(_, body)| body.0.error)?;
    if drift_accept {
        job.mode = "trigger_replay_drift_accept".to_string();
        job.target_label = format!("trigger replay {} (drift accepted)", entry.event_id);
    }
    Ok(job)
}

async fn load_normalized_dlq(event_log: &Arc<AnyEventLog>) -> Result<Vec<PortalDlqEntry>, String> {
    let topic = Topic::new(harn_vm::TRIGGER_DLQ_TOPIC).map_err(|error| error.to_string())?;
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())?;
    let mut latest = BTreeMap::<String, PortalDlqEntry>::new();
    for (_, event) in events {
        if !matches!(event.kind.as_str(), "dlq_moved" | "dlq_entry") {
            continue;
        }
        if let Some(entry) = normalize_dlq_event(&event) {
            latest.insert(entry.id.clone(), entry);
        }
    }
    let mut entries = latest.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .failed_at_ms
            .cmp(&left.failed_at_ms)
            .then(left.id.cmp(&right.id))
    });
    Ok(entries)
}

fn normalize_dlq_event(event: &LogEvent) -> Option<PortalDlqEntry> {
    let payload = &event.payload;
    if payload.get("id").and_then(JsonValue::as_str).is_some() && payload.get("event_id").is_some()
    {
        return normalize_stdlib_entry(event);
    }
    normalize_dispatcher_entry(event)
}

fn normalize_stdlib_entry(event: &LogEvent) -> Option<PortalDlqEntry> {
    let payload = &event.payload;
    let id = json_str(payload, "id")?;
    let event_id = json_str(payload, "event_id")?;
    let binding_id = json_str(payload, "binding_id").unwrap_or_else(|| "-".to_string());
    let binding_version = json_u32(payload, "binding_version");
    let binding_key = binding_version
        .map(|version| format!("{binding_id}@v{version}"))
        .unwrap_or_else(|| binding_id.clone());
    let event_json = payload.get("event").cloned().unwrap_or(JsonValue::Null);
    let last_error = json_str(payload, "error").unwrap_or_else(|| "unknown error".to_string());
    let attempt_history = payload
        .get("retry_history")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(PortalDlqAttempt {
                        attempt: json_u32(item, "attempt")?,
                        at: json_str(item, "at").unwrap_or_else(|| event_time(event)),
                        status: json_str(item, "status").unwrap_or_else(|| "dlq".to_string()),
                        error: item
                            .get("error")
                            .and_then(JsonValue::as_str)
                            .map(str::to_string),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let failed_at = attempt_history
        .last()
        .map(|attempt| attempt.at.clone())
        .unwrap_or_else(|| event_time(event));
    Some(PortalDlqEntry {
        id,
        event_id,
        trigger_id: binding_id.clone(),
        binding_id,
        binding_key,
        binding_version,
        provider: json_str(payload, "provider")
            .or_else(|| {
                event_json
                    .get("provider")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "-".to_string()),
        event_kind: json_str(payload, "kind")
            .or_else(|| {
                event_json
                    .get("kind")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "-".to_string()),
        failed_at_ms: parse_time_ms(&failed_at).unwrap_or(event.occurred_at_ms),
        failed_at,
        last_error: last_error.clone(),
        error_class: json_str(payload, "error_class")
            .unwrap_or_else(|| harn_vm::classify_trigger_dlq_error(&last_error).to_string()),
        retry_count: attempt_history.len() as u32,
        state: json_str(payload, "state").unwrap_or_else(|| "pending".to_string()),
        headers: json_headers(&event_json),
        payload: event_json
            .get("provider_payload")
            .cloned()
            .unwrap_or_else(|| event_json.clone()),
        event: event_json,
        attempt_history,
        predicate_trace: Vec::new(),
    })
}

fn normalize_dispatcher_entry(event: &LogEvent) -> Option<PortalDlqEntry> {
    let payload = &event.payload;
    let event_json = payload.get("event").cloned().unwrap_or(JsonValue::Null);
    let event_id = event_json
        .get("id")
        .and_then(JsonValue::as_str)
        .or_else(|| payload.get("event_id").and_then(JsonValue::as_str))?
        .to_string();
    let trigger_id = json_str(payload, "trigger_id").unwrap_or_else(|| "-".to_string());
    let binding_key = json_str(payload, "binding_key").unwrap_or_else(|| trigger_id.clone());
    let binding_id = binding_key
        .split_once("@v")
        .map(|(id, _)| id.to_string())
        .unwrap_or_else(|| trigger_id.clone());
    let binding_version = binding_key
        .split_once("@v")
        .and_then(|(_, version)| version.parse::<u32>().ok());
    let last_error =
        json_str(payload, "final_error").unwrap_or_else(|| "unknown error".to_string());
    let attempt_history = payload
        .get("attempts")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| PortalDlqAttempt {
                    attempt: json_u32(item, "attempt").unwrap_or(0),
                    at: json_str(item, "completed_at")
                        .or_else(|| json_str(item, "started_at"))
                        .unwrap_or_else(|| event_time(event)),
                    status: json_str(item, "outcome").unwrap_or_else(|| "failed".to_string()),
                    error: item
                        .get("error_msg")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let failed_at = attempt_history
        .last()
        .map(|attempt| attempt.at.clone())
        .unwrap_or_else(|| event_time(event));
    Some(PortalDlqEntry {
        id: stable_dlq_id(&binding_key, &event_id),
        event_id,
        trigger_id,
        binding_id,
        binding_key,
        binding_version,
        provider: event_json
            .get("provider")
            .and_then(JsonValue::as_str)
            .unwrap_or("-")
            .to_string(),
        event_kind: event_json
            .get("kind")
            .and_then(JsonValue::as_str)
            .unwrap_or("-")
            .to_string(),
        failed_at_ms: parse_time_ms(&failed_at).unwrap_or(event.occurred_at_ms),
        failed_at,
        last_error: last_error.clone(),
        error_class: json_str(payload, "error_class")
            .unwrap_or_else(|| harn_vm::classify_trigger_dlq_error(&last_error).to_string()),
        retry_count: json_u32(payload, "attempt_count").unwrap_or(attempt_history.len() as u32),
        state: "pending".to_string(),
        headers: json_headers(&event_json),
        payload: event_json
            .get("provider_payload")
            .cloned()
            .unwrap_or_else(|| event_json.clone()),
        event: event_json,
        attempt_history,
        predicate_trace: Vec::new(),
    })
}

async fn attach_predicate_trace(
    event_log: &Arc<AnyEventLog>,
    entries: &mut [PortalDlqEntry],
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let topic = Topic::new(harn_vm::TRIGGERS_LIFECYCLE_TOPIC).map_err(|error| error.to_string())?;
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())?;
    let mut by_event_id = HashMap::<String, Vec<JsonValue>>::new();
    for (_, event) in events {
        if !event.kind.starts_with("predicate.") {
            continue;
        }
        let Some(event_id) = event.headers.get("event_id").cloned().or_else(|| {
            event
                .payload
                .get("event_id")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        }) else {
            continue;
        };
        by_event_id
            .entry(event_id)
            .or_default()
            .push(serde_json::json!({
                "kind": event.kind,
                "occurred_at": event_time(&event),
                "headers": event.headers,
                "payload": event.payload,
            }));
    }
    for entry in entries {
        entry.predicate_trace = by_event_id.remove(&entry.event_id).unwrap_or_default();
    }
    Ok(())
}

fn filter_entries(
    entries: Vec<PortalDlqEntry>,
    query: &DlqQuery,
) -> Result<Vec<PortalDlqEntry>, String> {
    let since = query.since.as_deref().map(parse_time_ms).transpose()?;
    let until = query.until.as_deref().map(parse_time_ms).transpose()?;
    let state = query.state.as_deref().unwrap_or("pending");
    let q = query.q.as_deref().map(str::to_ascii_lowercase);
    Ok(entries
        .into_iter()
        .filter(|entry| state == "all" || entry.state == state)
        .filter(|entry| {
            query
                .trigger_id
                .as_ref()
                .is_none_or(|value| &entry.trigger_id == value || &entry.binding_id == value)
        })
        .filter(|entry| {
            query
                .provider
                .as_ref()
                .is_none_or(|value| &entry.provider == value)
        })
        .filter(|entry| {
            query
                .error_class
                .as_ref()
                .is_none_or(|value| &entry.error_class == value)
        })
        .filter(|entry| since.is_none_or(|value| entry.failed_at_ms >= value))
        .filter(|entry| until.is_none_or(|value| entry.failed_at_ms <= value))
        .filter(|entry| {
            q.as_ref().is_none_or(|needle| {
                entry.id.to_ascii_lowercase().contains(needle)
                    || entry.event_id.to_ascii_lowercase().contains(needle)
                    || entry.binding_id.to_ascii_lowercase().contains(needle)
                    || entry.last_error.to_ascii_lowercase().contains(needle)
            })
        })
        .collect())
}

fn group_by_error_class(entries: &[PortalDlqEntry]) -> Vec<PortalDlqGroup> {
    let mut groups = BTreeMap::<String, PortalDlqGroup>::new();
    for entry in entries {
        let group = groups
            .entry(entry.error_class.clone())
            .or_insert_with(|| PortalDlqGroup {
                error_class: entry.error_class.clone(),
                count: 0,
                newest_failed_at: None,
            });
        group.count += 1;
        if group
            .newest_failed_at
            .as_deref()
            .and_then(parse_time_ms_ok)
            .is_none_or(|current| entry.failed_at_ms > current)
        {
            group.newest_failed_at = Some(entry.failed_at.clone());
        }
    }
    groups.into_values().collect()
}

fn active_alerts(
    entries: &[PortalDlqEntry],
    configs: &[PortalDlqAlertConfig],
) -> Vec<PortalDlqAlert> {
    let now = OffsetDateTime::now_utc()
        .unix_timestamp()
        .saturating_mul(1000);
    let window_ms = 60 * 60 * 1000;
    let mut counts = BTreeMap::<(String, String), usize>::new();
    for entry in entries {
        if now.saturating_sub(entry.failed_at_ms) > window_ms {
            continue;
        }
        *counts
            .entry((entry.trigger_id.clone(), entry.error_class.clone()))
            .or_default() += 1;
    }
    let mut alerts = Vec::new();
    for ((trigger_id, error_class), count) in counts {
        let config = configs
            .iter()
            .find(|config| config.trigger_id == trigger_id);
        let threshold = config
            .and_then(|config| config.threshold_entries)
            .unwrap_or(5);
        if count >= threshold as usize {
            alerts.push(PortalDlqAlert {
                trigger_id,
                error_class,
                count,
                window_seconds: 3600,
                threshold_entries: threshold,
                destinations: config
                    .map(|config| config.destinations.clone())
                    .unwrap_or_default(),
            });
        }
    }
    alerts
}

fn load_alert_configs(workspace_root: &Path) -> Vec<PortalDlqAlertConfig> {
    let manifest_path = workspace_root.join("harn.toml");
    let Ok(source) = std::fs::read_to_string(manifest_path) else {
        return Vec::new();
    };
    let Ok(manifest) = toml::from_str::<crate::package::Manifest>(&source) else {
        return Vec::new();
    };
    manifest
        .triggers
        .into_iter()
        .flat_map(|trigger| {
            trigger.dlq_alerts.into_iter().map(move |alert| {
                let destinations = alert
                    .destinations
                    .into_iter()
                    .map(|destination| destination.label())
                    .collect::<Vec<_>>();
                PortalDlqAlertConfig {
                    trigger_id: trigger.id.clone(),
                    destinations,
                    threshold_entries: alert.threshold.entries_in_1h,
                    threshold_percent: alert.threshold.percent_of_dispatches,
                }
            })
        })
        .collect()
}

async fn append_portal_entry(
    state: &Arc<PortalState>,
    entry: &PortalDlqEntry,
    kind: &str,
) -> Result<(), String> {
    let event_log = state
        .event_log
        .as_ref()
        .ok_or_else(|| "portal is not attached to an event log".to_string())?;
    let topic = Topic::new(harn_vm::TRIGGER_DLQ_TOPIC).map_err(|error| error.to_string())?;
    let payload = serde_json::json!({
        "id": entry.id,
        "event_id": entry.event_id,
        "binding_id": entry.binding_id,
        "binding_version": entry.binding_version.unwrap_or_default(),
        "provider": entry.provider,
        "kind": entry.event_kind,
        "state": entry.state,
        "error": entry.last_error,
        "error_class": entry.error_class,
        "event": entry.event,
        "retry_history": entry.attempt_history,
    });
    event_log
        .append(&topic, LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn json_str(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn json_u32(value: &JsonValue, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(JsonValue::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn json_headers(event: &JsonValue) -> BTreeMap<String, String> {
    event
        .get("headers")
        .and_then(JsonValue::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn stable_dlq_id(binding_key: &str, event_id: &str) -> String {
    format!("dlq_{}_{}", sanitize_id(binding_key), sanitize_id(event_id))
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn event_time(event: &LogEvent) -> String {
    format_ms(event.occurred_at_ms)
}

use crate::format::format_unix_ms_rfc3339 as format_ms;

fn parse_time_ms(raw: &str) -> Result<i64, String> {
    parse_time_ms_ok(raw).ok_or_else(|| format!("invalid RFC3339 timestamp '{raw}'"))
}

fn parse_time_ms_ok(raw: &str) -> Option<i64> {
    OffsetDateTime::parse(raw, &Rfc3339)
        .ok()
        .map(|time| time.unix_timestamp().saturating_mul(1000))
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn sanitize_rate_limit(value: Option<f64>) -> f64 {
    value
        .unwrap_or(DEFAULT_BULK_RATE_LIMIT_PER_SECOND)
        .clamp(0.1, MAX_BULK_RATE_LIMIT_PER_SECOND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dispatcher_dlq_entries_with_error_class() {
        let event = LogEvent::new(
            "dlq_moved",
            serde_json::json!({
                "trigger_id": "cake-classifier",
                "binding_key": "cake-classifier@v2",
                "attempt_count": 3,
                "final_error": "provider returned 503 service unavailable",
                "event": {
                    "id": "trigger_evt_1",
                    "provider": "github",
                    "kind": "issues.opened",
                    "headers": {"x-delivery": "abc"},
                    "provider_payload": {"issue": {"number": 7}}
                },
                "attempts": [
                    {"attempt": 1, "completed_at": "2026-04-24T10:00:00Z", "outcome": "failed", "error_msg": "500"},
                    {"attempt": 2, "completed_at": "2026-04-24T10:01:00Z", "outcome": "failed", "error_msg": "503"}
                ]
            }),
        );

        let entry = normalize_dlq_event(&event).unwrap();
        assert_eq!(entry.id, "dlq_cake_classifier_v2_trigger_evt_1");
        assert_eq!(entry.error_class, "provider_5xx");
        assert_eq!(entry.retry_count, 3);
        assert_eq!(entry.headers["x-delivery"], "abc");
        assert_eq!(entry.payload["issue"]["number"], 7);
    }

    #[test]
    fn filters_dlq_entries_by_class_and_window() {
        let entries = vec![PortalDlqEntry {
            id: "dlq_1".to_string(),
            event_id: "evt".to_string(),
            trigger_id: "trigger".to_string(),
            binding_id: "trigger".to_string(),
            binding_key: "trigger@v1".to_string(),
            binding_version: Some(1),
            provider: "github".to_string(),
            event_kind: "issue".to_string(),
            failed_at: "2026-04-24T10:00:00Z".to_string(),
            failed_at_ms: parse_time_ms("2026-04-24T10:00:00Z").unwrap(),
            last_error: "boom".to_string(),
            error_class: "handler_panic".to_string(),
            retry_count: 1,
            state: "pending".to_string(),
            headers: BTreeMap::new(),
            payload: JsonValue::Null,
            event: JsonValue::Null,
            attempt_history: Vec::new(),
            predicate_trace: Vec::new(),
        }];

        let filtered = filter_entries(
            entries,
            &DlqQuery {
                error_class: Some("handler_panic".to_string()),
                since: Some("2026-04-24T09:00:00Z".to_string()),
                ..DlqQuery::default()
            },
        )
        .unwrap();
        assert_eq!(filtered.len(), 1);
    }
}
