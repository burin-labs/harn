use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harn_vm::event_log::{AnyEventLog, EventId, EventLog, LogEvent, Topic};
use serde::Serialize;

use crate::cli::PersonaSupervisionTailArgs;
use crate::package::{self, PersonaValidationError, ResolvedPersonaManifest};

use super::persona::{open_persona_log, timestamp_arg};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PersonaSupervisionTailFrame {
    pub event_id: EventId,
    pub persona_id: String,
    pub persona_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona_version: Option<String>,
    pub actor: String,
    pub update_kind: String,
    pub occurred_at: String,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonaSupervisionTailOptions {
    pub persona: Option<String>,
    pub since_event_id: Option<EventId>,
    pub at: Option<String>,
    pub follow: bool,
    pub limit: Option<usize>,
}

impl From<&PersonaSupervisionTailArgs> for PersonaSupervisionTailOptions {
    fn from(args: &PersonaSupervisionTailArgs) -> Self {
        Self {
            persona: args.persona.clone(),
            since_event_id: args.since_event_id,
            at: args.at.clone(),
            follow: args.follow,
            limit: args.limit,
        }
    }
}

/// In-process variant of `harn persona supervision tail` for finite reads.
pub async fn tail_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    options: &PersonaSupervisionTailOptions,
) -> Result<Vec<PersonaSupervisionTailFrame>, String> {
    validate_tail_options(options)?;
    let catalog = load_catalog_for_tail(manifest)?;
    let log = open_persona_log(state_dir)?;
    let topic = persona_runtime_topic()?;
    let mut cursor = options.since_event_id;
    let mut remaining = options.limit;
    read_supervision_frames(
        &log,
        &topic,
        &catalog,
        options.persona.as_deref(),
        &mut cursor,
        &mut remaining,
    )
    .await
}

pub(crate) async fn run_tail(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaSupervisionTailArgs,
) -> Result<(), String> {
    let options = PersonaSupervisionTailOptions::from(args);
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    drive_tail(manifest, state_dir, &options, &mut stdout, None).await
}

/// Drive the supervision tail loop against `writer`, optionally signalling
/// `ready_tx` once the watcher is armed and the initial frames have been
/// flushed. The CLI entrypoint passes a stdout lock and `None`; tests can
/// inject an in-memory writer plus a oneshot to avoid subprocess timing
/// races.
pub async fn drive_tail<W: std::io::Write>(
    manifest: Option<&Path>,
    state_dir: &Path,
    options: &PersonaSupervisionTailOptions,
    writer: &mut W,
    mut ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<(), String> {
    validate_tail_options(options)?;
    let catalog = load_catalog_for_tail(manifest)?;
    let log = open_persona_log(state_dir)?;
    let topic = persona_runtime_topic()?;
    let mut cursor = options.since_event_id;
    let mut remaining = options.limit;
    let mut waiter = EventLogChangeWaiter::new(log.describe().location);

    loop {
        let frames = read_supervision_frames(
            &log,
            &topic,
            &catalog,
            options.persona.as_deref(),
            &mut cursor,
            &mut remaining,
        )
        .await?;
        for frame in frames {
            let line = serde_json::to_string(&frame)
                .map_err(|error| format!("failed to serialize supervision frame: {error}"))?;
            writeln!(writer, "{line}")
                .map_err(|error| format!("failed to write supervision frame: {error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("failed to flush supervision stream: {error}"))?;

        if remaining == Some(0) || !options.follow {
            return Ok(());
        }
        if let Some(tx) = ready_tx.take() {
            let _ = tx.send(());
        }
        waiter.wait().await;
    }
}

fn load_catalog_for_tail(
    manifest: Option<&Path>,
) -> Result<Option<ResolvedPersonaManifest>, String> {
    if let Some(path) = manifest {
        return package::load_personas_from_manifest_path(path)
            .map(Some)
            .map_err(|errors| validation_errors_to_string(&errors));
    }
    package::load_personas_config(None).map_err(|errors| validation_errors_to_string(&errors))
}

fn validation_errors_to_string(errors: &[PersonaValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_tail_options(options: &PersonaSupervisionTailOptions) -> Result<(), String> {
    if let Some(at) = options.at.as_deref() {
        let _ = timestamp_arg(Some(at))?;
    }
    Ok(())
}

async fn read_supervision_frames(
    log: &Arc<AnyEventLog>,
    topic: &Topic,
    catalog: &Option<ResolvedPersonaManifest>,
    persona_filter: Option<&str>,
    cursor: &mut Option<EventId>,
    remaining: &mut Option<usize>,
) -> Result<Vec<PersonaSupervisionTailFrame>, String> {
    if remaining.as_ref() == Some(&0) {
        return Ok(Vec::new());
    }

    let raw_events = log
        .read_range(topic, *cursor, usize::MAX)
        .await
        .map_err(|error| error.to_string())?;
    let mut frames = Vec::new();
    for (event_id, event) in raw_events {
        *cursor = Some(event_id);
        if let Some(frame) = supervision_frame_from_event(event_id, event, catalog, persona_filter)?
        {
            frames.push(frame);
            if let Some(left) = remaining.as_mut() {
                *left = left.saturating_sub(1);
                if *left == 0 {
                    break;
                }
            }
        }
    }
    Ok(frames)
}

fn supervision_frame_from_event(
    event_id: EventId,
    event: LogEvent,
    catalog: &Option<ResolvedPersonaManifest>,
    persona_filter: Option<&str>,
) -> Result<Option<PersonaSupervisionTailFrame>, String> {
    let persona = event
        .headers
        .get("persona")
        .cloned()
        .or_else(|| {
            event
                .payload
                .get("persona_id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .payload
                .get("persona")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        });
    let Some(persona) = persona else {
        return Ok(None);
    };
    if persona_filter.is_some_and(|filter| filter != persona) {
        return Ok(None);
    }

    match event.kind.as_str() {
        kind if kind.starts_with("persona.supervision.") => {
            let update: harn_vm::PersonaSupervisionEvent = serde_json::from_value(event.payload)
                .map_err(|error| {
                    format!(
                        "failed to decode persisted persona supervision event {event_id}: {error}"
                    )
                })?;
            let persona_id = update.persona_id().to_string();
            if persona_filter.is_some_and(|filter| filter != persona_id) {
                return Ok(None);
            }
            let metadata = persona_metadata(catalog, &persona_id, update.template_ref());
            let frame = PersonaSupervisionTailFrame {
                event_id,
                persona_id,
                persona_kind: metadata.kind,
                persona_version: metadata.version,
                actor: "runtime".to_string(),
                update_kind: update.update_kind().to_string(),
                occurred_at: harn_vm::format_persona_ms(update.occurred_at_ms()),
                payload: supervision_update_payload(update)?,
            };
            Ok(Some(frame))
        }
        "persona.control.paused" | "persona.control.resumed" | "persona.control.disabled" => {
            let action = match event.kind.as_str() {
                "persona.control.paused" => "pause",
                "persona.control.resumed" => "resume",
                "persona.control.disabled" => "disable",
                _ => unreachable!("control event kind was matched above"),
            };
            let metadata = persona_metadata(catalog, &persona, None);
            Ok(Some(PersonaSupervisionTailFrame {
                event_id,
                persona_id: persona,
                persona_kind: metadata.kind,
                persona_version: metadata.version,
                actor: "runtime".to_string(),
                update_kind: "control".to_string(),
                occurred_at: harn_vm::format_persona_ms(event.occurred_at_ms),
                payload: serde_json::json!({
                    "action": action,
                    "metadata": event.payload,
                }),
            }))
        }
        "persona.trigger.queued" => {
            let Some(payload) = handoff_payload_from_queued_event(&event)? else {
                return Ok(None);
            };
            let metadata = persona_metadata(catalog, &persona, None);
            Ok(Some(PersonaSupervisionTailFrame {
                event_id,
                persona_id: persona,
                persona_kind: metadata.kind,
                persona_version: metadata.version,
                actor: "runtime".to_string(),
                update_kind: "handoff".to_string(),
                occurred_at: harn_vm::format_persona_ms(event.occurred_at_ms),
                payload,
            }))
        }
        _ => Ok(None),
    }
}

fn supervision_update_payload(
    update: harn_vm::PersonaSupervisionEvent,
) -> Result<serde_json::Value, String> {
    match update {
        harn_vm::PersonaSupervisionEvent::QueuePosition(update) => Ok(serde_json::json!({
            "work_key": update.work_key,
            "queue_depth": update.queue_depth,
            "position": update.position,
            "queued_at_ms": update.queued_at_ms,
            "queued_at": harn_vm::format_persona_ms(update.queued_at_ms),
        })),
        harn_vm::PersonaSupervisionEvent::RepairWorkerStatus(update) => Ok(serde_json::json!({
            "repair_worker_id": update.repair_worker_id,
            "lifecycle": update.lifecycle.as_str(),
            "work_key": update.work_key,
            "lease_id": update.lease_id,
            "scratchpad_url": update.scratchpad_url,
            "last_heartbeat_ms": update.last_heartbeat_ms,
            "last_heartbeat_at": harn_vm::format_persona_ms(update.last_heartbeat_ms),
        })),
        harn_vm::PersonaSupervisionEvent::Receipt(update) => {
            serde_json::to_value(update.receipt).map_err(|error| error.to_string())
        }
        harn_vm::PersonaSupervisionEvent::Checkpoint(update) => Ok(serde_json::json!({
            "action": update.action.as_str(),
            "checkpoint_id": update.checkpoint_id,
            "work_key": update.work_key,
            "resumed_from": update.resumed_from,
        })),
    }
}

fn handoff_payload_from_queued_event(
    event: &LogEvent,
) -> Result<Option<serde_json::Value>, String> {
    let Some(envelope) = event.payload.get("envelope") else {
        return Ok(None);
    };
    let envelope: harn_vm::PersonaTriggerEnvelope =
        serde_json::from_value(envelope.clone()).map_err(|error| error.to_string())?;
    if envelope.provider != "handoff" && !envelope.metadata.contains_key("handoff_id") {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "status": "queued",
        "work_key": envelope.subject_key,
        "handoff_id": envelope.metadata.get("handoff_id"),
        "handoff_kind": envelope
            .metadata
            .get("handoff_kind")
            .or_else(|| envelope.metadata.get("kind")),
        "source_persona": envelope.metadata.get("source_persona"),
        "task": envelope.metadata.get("task"),
        "reason": event
            .payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("queued"),
        "queued_at": harn_vm::format_persona_ms(event.occurred_at_ms),
    })))
}

struct PersonaProjectionMetadata {
    kind: String,
    version: Option<String>,
}

fn persona_metadata(
    catalog: &Option<ResolvedPersonaManifest>,
    persona_id: &str,
    template_ref: Option<&str>,
) -> PersonaProjectionMetadata {
    if let Some(persona) = catalog.as_ref().and_then(|catalog| {
        catalog
            .personas
            .iter()
            .find(|persona| persona.name.as_deref() == Some(persona_id))
    }) {
        return PersonaProjectionMetadata {
            kind: persona
                .name
                .clone()
                .unwrap_or_else(|| persona_id.to_string()),
            version: persona.version.clone(),
        };
    }
    PersonaProjectionMetadata {
        kind: persona_id.to_string(),
        version: template_ref.and_then(version_from_template_ref),
    }
}

fn version_from_template_ref(template_ref: &str) -> Option<String> {
    let (_, version) = template_ref.rsplit_once('@')?;
    if version.trim().is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

fn persona_runtime_topic() -> Result<Topic, String> {
    Topic::new(harn_vm::PERSONA_RUNTIME_TOPIC).map_err(|error| error.to_string())
}

struct EventLogChangeWaiter {
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl EventLogChangeWaiter {
    fn new(location: Option<PathBuf>) -> Self {
        let Some(location) = location else {
            return Self::polling();
        };
        let watch_path = if location.is_dir() {
            location
        } else if let Some(parent) = location.parent() {
            parent.to_path_buf()
        } else {
            location
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if result.is_ok() {
                let _ = tx.send(());
            }
        });
        let Ok(mut watcher) = watcher else {
            return Self::polling();
        };
        {
            use notify::Watcher as _;
            if watcher
                .watch(&watch_path, notify::RecursiveMode::NonRecursive)
                .is_err()
            {
                return Self::polling();
            }
        }
        Self {
            rx: Some(rx),
            _watcher: Some(watcher),
        }
    }

    fn polling() -> Self {
        Self {
            rx: None,
            _watcher: None,
        }
    }

    async fn wait(&mut self) {
        let Some(rx) = self.rx.as_mut() else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            return;
        };
        tokio::select! {
            _ = rx.recv() => {}
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
        while rx.try_recv().is_ok() {}
    }
}
