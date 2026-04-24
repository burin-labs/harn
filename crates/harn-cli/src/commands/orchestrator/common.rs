use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::OrchestratorLocalArgs;
use crate::package::{self, CollectedManifestTrigger, Manifest};

use super::role::OrchestratorRole;

pub(crate) const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";
pub(crate) use harn_vm::{
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_OUTBOX_TOPIC,
};

pub(crate) struct LoadedOrchestratorContext {
    pub vm: harn_vm::Vm,
    pub event_log: Arc<AnyEventLog>,
    pub collected_triggers: Vec<CollectedManifestTrigger>,
    pub snapshot: Option<PersistedStateSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PersistedStateSnapshot {
    pub status: String,
    pub bind: String,
    #[serde(default)]
    pub triggers: Vec<PersistedTriggerStateSnapshot>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub activations: Vec<ConnectorActivationSnapshot>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct PersistedTriggerStateSnapshot {
    pub id: String,
    pub provider: String,
    pub kind: String,
    pub handler: String,
    pub version: Option<u32>,
    pub state: Option<String>,
    #[serde(default)]
    pub received: u64,
    #[serde(default)]
    pub dispatched: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub in_flight: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConnectorActivationSnapshot {
    pub provider: String,
    pub binding_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DispatchHandleRecord {
    pub event_id: String,
    pub binding_id: String,
    pub binding_version: u32,
    pub status: String,
    pub replay_of_event_id: Option<String>,
    pub dlq_entry_id: Option<String>,
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DlqAttemptRecord {
    pub attempt: u32,
    pub at: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DlqEntryRecord {
    pub id: String,
    pub event_id: String,
    pub binding_id: String,
    pub binding_version: u32,
    pub provider: String,
    pub kind: String,
    pub state: String,
    pub error: String,
    pub event: harn_vm::TriggerEvent,
    pub retry_history: Vec<DlqAttemptRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StrandedEnvelopeRecord {
    pub inbox_offset: u64,
    pub event_id: String,
    pub trigger_id: Option<String>,
    pub binding_version: Option<u32>,
    pub provider: String,
    pub kind: String,
    pub received_at: OffsetDateTime,
    pub age: StdDuration,
}

pub(crate) async fn load_local_runtime(
    args: &OrchestratorLocalArgs,
) -> Result<LoadedOrchestratorContext, String> {
    harn_vm::reset_thread_local_state();

    let config_path = absolutize_from_cwd(&args.config)?;
    let (_manifest, manifest_dir) = load_manifest(&config_path)?;
    let state_dir = absolutize_from_cwd(&args.state_dir)?;
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create state dir {}: {error}",
            state_dir.display()
        )
    })?;

    let mut vm =
        OrchestratorRole::SingleTenant.build_vm(&manifest_dir, &manifest_dir, &state_dir)?;
    let extensions = package::load_runtime_extensions(&config_path);
    package::install_orchestrator_budget(&extensions);
    let collected_triggers = package::collect_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to collect manifest triggers: {error}"))?;
    package::install_collected_manifest_triggers(&collected_triggers).await?;

    let event_log = harn_vm::event_log::active_event_log()
        .ok_or_else(|| "event log was not installed during VM initialization".to_string())?;
    let snapshot = read_state_snapshot(&state_dir.join(STATE_SNAPSHOT_FILE))?;

    Ok(LoadedOrchestratorContext {
        vm,
        event_log,
        collected_triggers,
        snapshot,
    })
}

pub(crate) async fn trigger_list(
    ctx: &mut LoadedOrchestratorContext,
) -> Result<Vec<harn_vm::TriggerBindingSnapshot>, String> {
    eval_json(&mut ctx.vm, "trigger_list()").await
}

pub(crate) async fn trigger_replay(
    ctx: &mut LoadedOrchestratorContext,
    event_id: &str,
) -> Result<DispatchHandleRecord, String> {
    let event_id = serde_json::to_string(event_id).map_err(|error| error.to_string())?;
    eval_json(&mut ctx.vm, &format!("trigger_replay({event_id})")).await
}

pub(crate) async fn trigger_inspect_dlq(
    ctx: &mut LoadedOrchestratorContext,
) -> Result<Vec<DlqEntryRecord>, String> {
    eval_json(&mut ctx.vm, "trigger_inspect_dlq()").await
}

pub(crate) async fn trigger_fire(
    ctx: &mut LoadedOrchestratorContext,
    binding_id: &str,
    event: serde_json::Value,
) -> Result<DispatchHandleRecord, String> {
    let binding_id = serde_json::to_string(binding_id).map_err(|error| error.to_string())?;
    let event_json = serde_json::to_string(&event).map_err(|error| error.to_string())?;
    let event_literal = serde_json::to_string(&event_json).map_err(|error| error.to_string())?;
    eval_json(
        &mut ctx.vm,
        &format!("trigger_fire({binding_id}, json_parse({event_literal}))"),
    )
    .await
}

pub(crate) fn synthetic_event_for_binding(
    ctx: &LoadedOrchestratorContext,
    binding_id: &str,
) -> Result<serde_json::Value, String> {
    let trigger = ctx
        .collected_triggers
        .iter()
        .find(|trigger| trigger.config.id == binding_id)
        .ok_or_else(|| format!("unknown manifest binding '{binding_id}'"))?;
    let kind = trigger
        .config
        .match_
        .events
        .first()
        .cloned()
        .unwrap_or_else(|| match trigger.config.kind {
            crate::package::TriggerKind::Webhook => "webhook".to_string(),
            crate::package::TriggerKind::Cron => "cron.tick".to_string(),
            crate::package::TriggerKind::Poll => "poll".to_string(),
            crate::package::TriggerKind::Stream => "stream".to_string(),
            crate::package::TriggerKind::Predicate => "predicate".to_string(),
            crate::package::TriggerKind::A2aPush => "a2a.task.received".to_string(),
        });
    Ok(json!({
        "provider": trigger.config.provider.as_str(),
        "kind": kind,
        "headers": {
            "x-harn-binding-id": binding_id,
            "x-harn-synthetic": "true",
        }
    }))
}

pub(crate) async fn read_topic(
    log: &Arc<AnyEventLog>,
    topic_name: &str,
) -> Result<Vec<(u64, LogEvent)>, String> {
    let topic = Topic::new(topic_name).map_err(|error| error.to_string())?;
    log.read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn stranded_envelopes(
    log: &Arc<AnyEventLog>,
    min_age: StdDuration,
) -> Result<Vec<StrandedEnvelopeRecord>, String> {
    let envelopes = read_topic(log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
    let legacy_inbox = read_topic(log, TRIGGER_INBOX_LEGACY_TOPIC).await?;
    let outbox = read_topic(log, TRIGGER_OUTBOX_TOPIC).await?;

    let mut any_outbox_event_ids = BTreeSet::new();
    let mut outbox_by_event_id = BTreeMap::<String, BTreeSet<String>>::new();
    for (_, event) in outbox {
        let Some(event_id) = event.headers.get("event_id").cloned() else {
            continue;
        };
        any_outbox_event_ids.insert(event_id.clone());
        if let Some(trigger_id) = event.headers.get("trigger_id").cloned() {
            outbox_by_event_id
                .entry(event_id)
                .or_default()
                .insert(trigger_id);
        }
    }

    let mut stranded = Vec::new();
    for (offset, event) in envelopes.into_iter().chain(legacy_inbox) {
        if event.kind != "event_ingested" {
            continue;
        }
        let envelope: harn_vm::triggers::dispatcher::InboxEnvelope =
            serde_json::from_value(event.payload)
                .map_err(|error| format!("failed to decode trigger inbox envelope: {error}"))?;
        let age = age_since(envelope.event.received_at);
        if age < min_age {
            continue;
        }

        let event_id = envelope.event.id.0.clone();
        let matched_outbox = if let Some(trigger_id) = envelope.trigger_id.as_ref() {
            outbox_by_event_id
                .get(&event_id)
                .is_some_and(|trigger_ids| trigger_ids.contains(trigger_id))
        } else {
            any_outbox_event_ids.contains(&event_id)
        };
        if matched_outbox {
            continue;
        }

        stranded.push(StrandedEnvelopeRecord {
            inbox_offset: offset,
            event_id,
            trigger_id: envelope.trigger_id,
            binding_version: envelope.binding_version,
            provider: envelope.event.provider.as_str().to_string(),
            kind: envelope.event.kind,
            received_at: envelope.event.received_at,
            age,
        });
    }

    stranded.sort_by(|left, right| {
        left.received_at
            .cmp(&right.received_at)
            .then(left.event_id.cmp(&right.event_id))
    });
    Ok(stranded)
}

pub(crate) async fn append_dlq_entry(
    log: &Arc<AnyEventLog>,
    entry: &DlqEntryRecord,
) -> Result<(), String> {
    let topic = Topic::new(TRIGGER_DLQ_TOPIC).map_err(|error| error.to_string())?;
    let payload = serde_json::to_value(entry).map_err(|error| error.to_string())?;
    log.append(&topic, LogEvent::new("dlq_entry", payload))
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn discard_dlq_entry(entry: &DlqEntryRecord) -> Result<DlqEntryRecord, String> {
    let mut next = entry.clone();
    next.state = "discarded".to_string();
    next.retry_history.push(DlqAttemptRecord {
        attempt: (next.retry_history.len() + 1) as u32,
        at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| error.to_string())?,
        status: "discarded".to_string(),
        error: None,
    });
    Ok(next)
}

pub(crate) fn print_json<T>(value: &T) -> Result<(), String>
where
    T: Serialize,
{
    let encoded = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    println!("{encoded}");
    Ok(())
}

fn read_state_snapshot(path: &Path) -> Result<Option<PersistedStateSnapshot>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let snapshot = serde_json::from_str(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    Ok(Some(snapshot))
}

async fn eval_json<T>(vm: &mut harn_vm::Vm, expr: &str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let source = format!("pipeline default() {{\n  return {expr}\n}}\n");
    let chunk = harn_vm::compile_source(&source)?;
    let value = vm
        .execute(&chunk)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_value(harn_vm::llm::vm_value_to_json(&value))
        .map_err(|error| format!("failed to decode builtin result: {error}"))
}

fn load_manifest(config_path: &Path) -> Result<(Manifest, PathBuf), String> {
    if !config_path.is_file() {
        return Err(format!("manifest not found: {}", config_path.display()));
    }
    let content = std::fs::read_to_string(config_path)
        .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
    let manifest = toml::from_str::<Manifest>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", config_path.display()))?;
    let manifest_dir = config_path.parent().map(Path::to_path_buf).ok_or_else(|| {
        format!(
            "manifest has no parent directory: {}",
            config_path.display()
        )
    })?;
    Ok((manifest, manifest_dir))
}

pub(crate) fn absolutize_from_cwd(path: &Path) -> Result<PathBuf, String> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to read current directory: {error}"))?
            .join(path)
    };
    Ok(candidate)
}

pub(super) fn format_timestamp(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

pub(super) fn format_duration(value: StdDuration) -> String {
    if value.as_secs() == 0 {
        return format!("{}ms", value.as_millis());
    }
    let seconds = value.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 60 * 60 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 60 * 60 * 24 {
        return format!("{}h", seconds / (60 * 60));
    }
    format!("{}d", seconds / (60 * 60 * 24))
}

fn age_since(then: OffsetDateTime) -> StdDuration {
    let now = OffsetDateTime::now_utc();
    if now <= then {
        return StdDuration::ZERO;
    }
    let delta = now - then;
    StdDuration::new(
        delta.whole_seconds() as u64,
        delta.subsec_nanoseconds().try_into().unwrap_or_default(),
    )
}
