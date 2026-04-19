use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::OrchestratorLocalArgs;
use crate::package::{self, CollectedManifestTrigger, Manifest};

use super::role::OrchestratorRole;

pub(super) const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";
pub(super) const TRIGGER_INBOX_TOPIC: &str = "trigger.inbox";
pub(super) const TRIGGER_OUTBOX_TOPIC: &str = "trigger.outbox";
pub(super) const TRIGGER_ATTEMPTS_TOPIC: &str = "trigger.attempts";
// Must match `harn_vm::triggers::dispatcher::TRIGGER_DLQ_TOPIC`. Previous
// value "triggers.dlq" silently diverged and made `harn orchestrator dlq`
// return zero results even when the dispatcher was actively writing DLQ
// entries.
pub(super) const TRIGGER_DLQ_TOPIC: &str = "trigger.dlq";

pub(super) struct LoadedOrchestratorContext {
    pub vm: harn_vm::Vm,
    pub event_log: Arc<AnyEventLog>,
    pub collected_triggers: Vec<CollectedManifestTrigger>,
    pub snapshot: Option<PersistedStateSnapshot>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct PersistedStateSnapshot {
    pub status: String,
    pub bind: String,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub activations: Vec<ConnectorActivationSnapshot>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ConnectorActivationSnapshot {
    pub provider: String,
    pub binding_count: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DispatchHandleRecord {
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
pub(super) struct DlqAttemptRecord {
    pub attempt: u32,
    pub at: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DlqEntryRecord {
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

pub(super) async fn load_local_runtime(
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

pub(super) async fn trigger_list(
    ctx: &mut LoadedOrchestratorContext,
) -> Result<Vec<harn_vm::TriggerBindingSnapshot>, String> {
    eval_json(&mut ctx.vm, "trigger_list()").await
}

pub(super) async fn trigger_replay(
    ctx: &mut LoadedOrchestratorContext,
    event_id: &str,
) -> Result<DispatchHandleRecord, String> {
    let event_id = serde_json::to_string(event_id).map_err(|error| error.to_string())?;
    eval_json(&mut ctx.vm, &format!("trigger_replay({event_id})")).await
}

pub(super) async fn trigger_inspect_dlq(
    ctx: &mut LoadedOrchestratorContext,
) -> Result<Vec<DlqEntryRecord>, String> {
    eval_json(&mut ctx.vm, "trigger_inspect_dlq()").await
}

pub(super) async fn trigger_fire(
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

pub(super) fn synthetic_event_for_binding(
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

pub(super) async fn read_topic(
    log: &Arc<AnyEventLog>,
    topic_name: &str,
) -> Result<Vec<(u64, LogEvent)>, String> {
    let topic = Topic::new(topic_name).map_err(|error| error.to_string())?;
    log.read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())
}

pub(super) async fn append_dlq_entry(
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

pub(super) fn discard_dlq_entry(entry: &DlqEntryRecord) -> Result<DlqEntryRecord, String> {
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

fn absolutize_from_cwd(path: &Path) -> Result<PathBuf, String> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to read current directory: {error}"))?
            .join(path)
    };
    Ok(candidate)
}
