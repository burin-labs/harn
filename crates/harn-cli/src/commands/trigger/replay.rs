use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};

use crate::cli::TriggerReplayArgs;
use crate::package;

const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
const TRIGGER_OUTBOX_TOPIC: &str = "trigger.outbox";
const TRIGGER_DLQ_TOPIC: &str = "trigger.dlq";
const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TriggerEventRecord {
    binding_id: String,
    binding_version: u32,
    replay_of_event_id: Option<String>,
    event: harn_vm::TriggerEvent,
}

#[derive(Clone, Debug, Serialize)]
struct DispatchOutcomeSummary {
    status: String,
    attempt_count: u32,
    handler_kind: String,
    target_uri: Option<String>,
    result: Option<JsonValue>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct DriftField {
    original: JsonValue,
    replayed: JsonValue,
}

#[derive(Clone, Debug, Serialize)]
struct DriftReport {
    changed: bool,
    fields: BTreeMap<String, DriftField>,
}

#[derive(Clone, Debug, Serialize)]
struct TriggerReplayReport {
    event_id: String,
    binding_id: String,
    binding_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    replay: DispatchOutcomeSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    original: Option<DispatchOutcomeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    drift: Option<DriftReport>,
}

pub(crate) async fn run(args: TriggerReplayArgs) -> Result<(), String> {
    harn_vm::reset_thread_local_state();

    let cwd = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let workspace_root = harn_vm::stdlib::process::find_project_root(&cwd).unwrap_or(cwd.clone());
    let event_log = harn_vm::event_log::install_default_for_base_dir(&workspace_root)
        .map_err(|error| format!("failed to open event log snapshot: {error}"))?;

    let mut vm = build_replay_vm(&workspace_root);
    let extensions = package::load_runtime_extensions(&workspace_root);
    package::install_runtime_extensions(&extensions);
    package::install_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to install manifest triggers: {error}"))?;

    let recorded = load_recorded_event(&event_log, &args.event_id).await?;
    let original = if args.diff {
        Some(load_original_outcome(&event_log, &recorded).await?)
    } else {
        None
    };
    let as_of = args.as_of.as_deref().map(parse_timestamp).transpose()?;
    let binding = resolve_binding(&recorded, as_of)?;

    append_replay_record(&event_log, &binding, &recorded.event).await?;
    let dispatcher = harn_vm::Dispatcher::with_event_log(vm, event_log);
    let replay = dispatcher
        .dispatch_replay(
            &binding,
            recorded.event.clone(),
            recorded.event.id.0.clone(),
        )
        .await
        .map_err(|error| format!("trigger replay failed: {error}"))?;
    let replay_summary = summarize_dispatch_outcome(&replay);

    let drift = original
        .as_ref()
        .map(|original| diff_outcomes(original, &replay_summary));
    let report = TriggerReplayReport {
        event_id: recorded.event.id.0,
        binding_id: binding.id.as_str().to_string(),
        binding_version: binding.version,
        as_of: as_of.map(format_timestamp),
        replay: replay_summary,
        original,
        drift,
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to encode replay report: {error}"))?
    );
    Ok(())
}

fn build_replay_vm(workspace_root: &Path) -> harn_vm::Vm {
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    harn_vm::register_store_builtins(&mut vm, workspace_root);
    harn_vm::register_metadata_builtins(&mut vm, workspace_root);
    harn_vm::register_checkpoint_builtins(&mut vm, workspace_root, "trigger-replay");
    vm.set_project_root(workspace_root);
    vm.set_source_dir(workspace_root);
    vm
}

fn parse_timestamp(raw: &str) -> Result<OffsetDateTime, String> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        };
        return parsed.map_err(|error| format!("invalid --as-of timestamp '{raw}': {error}"));
    }
    Err(format!(
        "invalid --as-of timestamp '{raw}': expected RFC3339 or unix seconds/milliseconds"
    ))
}

fn format_timestamp(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

async fn load_recorded_event(
    event_log: &Arc<AnyEventLog>,
    event_id: &str,
) -> Result<TriggerEventRecord, String> {
    let topic = Topic::new(TRIGGER_EVENTS_TOPIC)
        .map_err(|error| format!("invalid trigger events topic: {error}"))?;
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read trigger events: {error}"))?;

    let mut replay_match = None;
    for (_, event) in events {
        let Ok(record) = serde_json::from_value::<TriggerEventRecord>(event.payload) else {
            continue;
        };
        if record.event.id.0 != event_id {
            continue;
        }
        if record.replay_of_event_id.is_none() {
            return Ok(record);
        }
        replay_match.get_or_insert(record);
    }

    replay_match.ok_or_else(|| format!("unknown trigger event id '{event_id}'"))
}

fn resolve_binding(
    recorded: &TriggerEventRecord,
    as_of: Option<OffsetDateTime>,
) -> Result<Arc<harn_vm::triggers::registry::TriggerBinding>, String> {
    if let Some(as_of) = as_of {
        return harn_vm::resolve_trigger_binding_as_of(&recorded.binding_id, as_of).map_err(
            |error| {
                format!(
                    "failed to resolve binding '{}' as of {}: {}",
                    recorded.binding_id,
                    format_timestamp(as_of),
                    error
                )
            },
        );
    }

    harn_vm::resolve_live_or_as_of(
        &recorded.binding_id,
        harn_vm::RecordedTriggerBinding {
            version: recorded.binding_version,
            received_at: recorded.event.received_at,
        },
    )
    .map_err(|error| {
        format!(
            "failed to resolve recorded binding '{}@v{}' for replay: {}",
            recorded.binding_id, recorded.binding_version, error
        )
    })
}

async fn append_replay_record(
    event_log: &Arc<AnyEventLog>,
    binding: &harn_vm::triggers::registry::TriggerBinding,
    event: &harn_vm::TriggerEvent,
) -> Result<(), String> {
    let topic = Topic::new(TRIGGER_EVENTS_TOPIC)
        .map_err(|error| format!("invalid trigger events topic: {error}"))?;
    event_log
        .append(
            &topic,
            LogEvent::new(
                "trigger_event",
                serde_json::to_value(TriggerEventRecord {
                    binding_id: binding.id.as_str().to_string(),
                    binding_version: binding.version,
                    replay_of_event_id: Some(event.id.0.clone()),
                    event: event.clone(),
                })
                .unwrap_or(JsonValue::Null),
            ),
        )
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to append replay record: {error}"))
}

async fn load_original_outcome(
    event_log: &Arc<AnyEventLog>,
    recorded: &TriggerEventRecord,
) -> Result<DispatchOutcomeSummary, String> {
    let binding_key = format!("{}@v{}", recorded.binding_id, recorded.binding_version);
    if let Some(outcome) =
        load_original_terminal_outcome(event_log, &recorded.event.id.0, &binding_key).await?
    {
        return Ok(outcome);
    }

    load_skipped_outcome(event_log, &recorded.event.id.0, &binding_key)
        .await?
        .ok_or_else(|| {
            format!(
                "no stored original outcome found for '{}@v{}' event '{}'",
                recorded.binding_id, recorded.binding_version, recorded.event.id.0
            )
        })
}

async fn load_original_terminal_outcome(
    event_log: &Arc<AnyEventLog>,
    event_id: &str,
    binding_key: &str,
) -> Result<Option<DispatchOutcomeSummary>, String> {
    let outbox_topic = Topic::new(TRIGGER_OUTBOX_TOPIC)
        .map_err(|error| format!("invalid trigger outbox topic: {error}"))?;
    let dlq_topic = Topic::new(TRIGGER_DLQ_TOPIC)
        .map_err(|error| format!("invalid trigger dlq topic: {error}"))?;

    let outbox_events = event_log
        .read_range(&outbox_topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read trigger outbox: {error}"))?;
    let dlq_events = event_log
        .read_range(&dlq_topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read trigger dlq: {error}"))?;

    let mut success = None;
    let mut failure = None;
    for (_, event) in outbox_events {
        if !matches_original_dispatch(&event, event_id, binding_key) {
            continue;
        }
        let attempt = header_u32(&event, "attempt").unwrap_or(0);
        let handler_kind = header_text(&event, "handler_kind").unwrap_or_default();
        let target_uri = event
            .payload
            .get("target_uri")
            .cloned()
            .and_then(|value| value.as_str().map(str::to_string));
        match event.kind.as_str() {
            "dispatch_succeeded" => {
                success = Some(DispatchOutcomeSummary {
                    status: "succeeded".to_string(),
                    attempt_count: attempt,
                    handler_kind,
                    target_uri,
                    result: event.payload.get("result").cloned(),
                    error: None,
                });
            }
            "dispatch_failed" => {
                let error = event
                    .payload
                    .get("error")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                failure = Some(DispatchOutcomeSummary {
                    status: failure_status(error.as_deref()),
                    attempt_count: attempt,
                    handler_kind,
                    target_uri,
                    result: None,
                    error,
                });
            }
            _ => {}
        }
    }

    for (_, event) in dlq_events {
        if !matches_original_dispatch(&event, event_id, binding_key) || event.kind != "dlq_moved" {
            continue;
        }
        let attempt_count = event
            .payload
            .get("attempt_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32;
        return Ok(Some(DispatchOutcomeSummary {
            status: "dlq".to_string(),
            attempt_count,
            handler_kind: header_text(&event, "handler_kind").unwrap_or_default(),
            target_uri: None,
            result: None,
            error: event
                .payload
                .get("final_error")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }));
    }

    Ok(success.or(failure))
}

async fn load_skipped_outcome(
    event_log: &Arc<AnyEventLog>,
    event_id: &str,
    binding_key: &str,
) -> Result<Option<DispatchOutcomeSummary>, String> {
    let topic = Topic::new(ACTION_GRAPH_TOPIC)
        .map_err(|error| format!("invalid action graph topic: {error}"))?;
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read action graph updates: {error}"))?;

    for (_, event) in events {
        let Some(context) = event.payload.get("context") else {
            continue;
        };
        if context.get("event_id").and_then(|value| value.as_str()) != Some(event_id) {
            continue;
        }
        if context.get("binding_key").and_then(|value| value.as_str()) != Some(binding_key) {
            continue;
        }
        if context
            .get("replay_of_event_id")
            .and_then(|value| value.as_str())
            .is_some()
        {
            continue;
        }
        let Some(nodes) = event.payload["observability"]["action_graph_nodes"].as_array() else {
            continue;
        };
        let predicate = nodes.iter().find(|node| {
            node.get("kind").and_then(|value| value.as_str()) == Some("predicate")
                && node.get("outcome").and_then(|value| value.as_str()) == Some("false")
        });
        if let Some(predicate) = predicate {
            return Ok(Some(DispatchOutcomeSummary {
                status: "skipped".to_string(),
                attempt_count: 0,
                handler_kind: String::new(),
                target_uri: None,
                result: Some(json!({
                    "skipped": true,
                    "predicate": predicate.get("label").cloned().unwrap_or(JsonValue::Null),
                })),
                error: None,
            }));
        }
    }

    Ok(None)
}

fn matches_original_dispatch(event: &LogEvent, event_id: &str, binding_key: &str) -> bool {
    header_text(event, "event_id") == Some(event_id.to_string())
        && header_text(event, "binding_key") == Some(binding_key.to_string())
        && header_text(event, "replay_of_event_id").is_none()
}

fn header_text(event: &LogEvent, key: &str) -> Option<String> {
    event.headers.get(key).cloned()
}

fn header_u32(event: &LogEvent, key: &str) -> Option<u32> {
    event.headers.get(key).and_then(|value| value.parse().ok())
}

fn failure_status(error: Option<&str>) -> String {
    if error.is_some_and(|error| error.contains("cancelled")) {
        "cancelled".to_string()
    } else {
        "failed".to_string()
    }
}

fn summarize_dispatch_outcome(outcome: &harn_vm::DispatchOutcome) -> DispatchOutcomeSummary {
    DispatchOutcomeSummary {
        status: match outcome.status {
            harn_vm::DispatchStatus::Succeeded => "succeeded".to_string(),
            harn_vm::DispatchStatus::Failed => "failed".to_string(),
            harn_vm::DispatchStatus::Dlq => "dlq".to_string(),
            harn_vm::DispatchStatus::Skipped => "skipped".to_string(),
            harn_vm::DispatchStatus::Cancelled => "cancelled".to_string(),
        },
        attempt_count: outcome.attempt_count,
        handler_kind: outcome.handler_kind.clone(),
        target_uri: Some(outcome.target_uri.clone()),
        result: outcome.result.clone(),
        error: outcome.error.clone(),
    }
}

fn diff_outcomes(
    original: &DispatchOutcomeSummary,
    replayed: &DispatchOutcomeSummary,
) -> DriftReport {
    let original = serde_json::to_value(original).unwrap_or(JsonValue::Null);
    let replayed = serde_json::to_value(replayed).unwrap_or(JsonValue::Null);
    let mut fields = BTreeMap::new();

    let original = original.as_object().cloned().unwrap_or_default();
    let replayed = replayed.as_object().cloned().unwrap_or_default();
    let mut keys = original.keys().cloned().collect::<Vec<_>>();
    for key in replayed.keys() {
        if !keys.iter().any(|existing| existing == key) {
            keys.push(key.clone());
        }
    }
    keys.sort();
    keys.dedup();

    for key in keys {
        let left = original.get(&key).cloned().unwrap_or(JsonValue::Null);
        let right = replayed.get(&key).cloned().unwrap_or(JsonValue::Null);
        if left != right {
            fields.insert(
                key,
                DriftField {
                    original: left,
                    replayed: right,
                },
            );
        }
    }

    DriftReport {
        changed: !fields.is_empty(),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::Arc;

    use harn_vm::event_log::{
        install_default_for_base_dir, AnyEventLog, EventLog, LogEvent, Topic,
    };
    use harn_vm::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
    use harn_vm::triggers::event::{CronEventPayload, KnownProviderPayload};
    use time::OffsetDateTime;

    use super::{
        append_replay_record, build_replay_vm, load_recorded_event, resolve_binding,
        summarize_dispatch_outcome, TriggerEventRecord, TRIGGER_EVENTS_TOPIC,
    };
    use crate::package;

    const TEST_TRIGGER_ID: &str = "replay-cron";

    #[tokio::test(flavor = "current_thread")]
    async fn replay_falls_back_to_recorded_timestamp_when_version_lookup_is_stale() {
        harn_vm::reset_thread_local_state();
        let sink = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(sink.clone());

        let tempdir = tempfile::tempdir().expect("tempdir");
        let workspace_root = tempdir.path();
        let event_log = install_default_for_base_dir(workspace_root).expect("install event log");

        install_local_manifest(workspace_root, "on_tick_v1");
        install_workspace_manifest(workspace_root).await;
        install_local_manifest(workspace_root, "on_tick_v2");
        install_workspace_manifest(workspace_root).await;
        install_local_manifest(workspace_root, "on_tick_v3");
        install_workspace_manifest(workspace_root).await;

        let current = harn_vm::resolve_live_trigger_binding(TEST_TRIGGER_ID, None)
            .expect("resolve active binding");
        assert_eq!(current.version, 3);
        assert!(matches!(
            harn_vm::resolve_live_trigger_binding(TEST_TRIGGER_ID, Some(1)),
            Err(harn_vm::TriggerRegistryError::UnknownBindingVersion { .. })
        ));

        append_trigger_event(
            &event_log,
            TriggerEventRecord {
                binding_id: TEST_TRIGGER_ID.to_string(),
                binding_version: 1,
                replay_of_event_id: None,
                event: recorded_cron_event("evt-stale", OffsetDateTime::now_utc()),
            },
        )
        .await;

        let recorded = load_recorded_event(&event_log, "evt-stale")
            .await
            .expect("load recorded event");
        let binding = resolve_binding(&recorded, None).expect("resolve fallback binding");
        append_replay_record(&event_log, &binding, &recorded.event)
            .await
            .expect("append replay record");

        let dispatcher =
            harn_vm::Dispatcher::with_event_log(build_replay_vm(workspace_root), event_log.clone());
        let outcome = dispatcher
            .dispatch_replay(
                &binding,
                recorded.event.clone(),
                recorded.event.id.0.clone(),
            )
            .await
            .expect("dispatch replay succeeds");
        let replay = summarize_dispatch_outcome(&outcome);
        assert_eq!(replay.status, "succeeded");

        let topic = Topic::new(TRIGGER_EVENTS_TOPIC).expect("valid trigger events topic");
        let records: Vec<TriggerEventRecord> = event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .expect("read trigger events")
            .into_iter()
            .map(|(_, event)| serde_json::from_value(event.payload).expect("decode trigger event"))
            .collect();

        assert!(records.iter().any(|record| {
            record.replay_of_event_id.as_deref() == Some("evt-stale")
                && record.binding_id == TEST_TRIGGER_ID
                && record.binding_version == 3
        }));
        assert!(sink.logs.borrow().iter().any(|log| {
            log.level == EventLevel::Warn
                && log.category == "replay.binding_version_gc_fallback"
                && log.metadata.get("trigger_id") == Some(&serde_json::json!(TEST_TRIGGER_ID))
                && log.metadata.get("recorded_version") == Some(&serde_json::json!(1))
                && log.metadata.get("resolved_version") == Some(&serde_json::json!(3))
        }));

        harn_vm::reset_thread_local_state();
    }

    fn install_local_manifest(root: &Path, handler_name: &str) {
        std::fs::create_dir_all(root.join(".git")).expect("create .git");
        fs::write(
            root.join("harn.toml"),
            format!(
                r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "{TEST_TRIGGER_ID}"
kind = "cron"
provider = "cron"
match = {{ events = ["cron.tick"] }}
schedule = "* * * * *"
timezone = "UTC"
handler = "handlers::{handler_name}"
"#
            ),
        )
        .expect("write manifest");
        fs::write(
            root.join("lib.harn"),
            format!(
                r#"
import "std/triggers"

pub fn {handler_name}(event: TriggerEvent) -> string {{
  return event.kind
}}
"#
            ),
        )
        .expect("write lib");
        fs::write(root.join("main.harn"), "pipeline main() {}\n").expect("write main");
    }

    async fn install_workspace_manifest(root: &Path) {
        let mut vm = super::build_replay_vm(root);
        let extensions = package::load_runtime_extensions(&root.join("main.harn"));
        package::install_manifest_triggers(&mut vm, &extensions)
            .await
            .expect("install manifest triggers");
    }

    fn recorded_cron_event(event_id: &str, received_at: OffsetDateTime) -> harn_vm::TriggerEvent {
        harn_vm::TriggerEvent {
            id: harn_vm::TriggerEventId(event_id.to_string()),
            provider: harn_vm::ProviderId::from("cron"),
            kind: "cron.tick".to_string(),
            received_at,
            occurred_at: None,
            dedupe_key: format!("delivery-{event_id}"),
            trace_id: harn_vm::TraceId(format!("trace-{event_id}")),
            tenant_id: None,
            headers: BTreeMap::new(),
            provider_payload: harn_vm::ProviderPayload::Known(KnownProviderPayload::Cron(
                CronEventPayload {
                    cron_id: Some("test-cron".to_string()),
                    schedule: Some("* * * * *".to_string()),
                    tick_at: received_at,
                    raw: serde_json::json!({ "event_id": event_id }),
                },
            )),
            signature_status: harn_vm::SignatureStatus::Verified,
            dedupe_claimed: false,
        }
    }

    async fn append_trigger_event(event_log: &Arc<AnyEventLog>, record: TriggerEventRecord) {
        let topic = Topic::new(TRIGGER_EVENTS_TOPIC).expect("valid trigger events topic");
        event_log
            .append(
                &topic,
                LogEvent::new(
                    "trigger_event",
                    serde_json::to_value(record).expect("encode trigger event"),
                ),
            )
            .await
            .expect("append trigger event");
    }
}
