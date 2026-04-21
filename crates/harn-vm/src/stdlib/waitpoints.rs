use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration as StdDuration;

use futures::{pin_mut, stream::SelectAll, StreamExt};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, AnyEventLog, EventLog,
};
use crate::llm::vm_value_to_json;
use crate::triggers::dispatcher::{current_dispatch_context, current_dispatch_wait_lease};
use crate::value::{VmError, VmValue};
use crate::vm::clone_async_builtin_child_vm;
use crate::vm::Vm;
use crate::waitpoints::{
    append_wait_started, append_wait_terminal, cancel_waitpoint, complete_waitpoint,
    create_waitpoint, dedupe_waitpoint_ids, find_wait_terminal, load_waitpoints,
    resolve_waitpoints, waitpoint_topic, WaitpointRecord, WaitpointResolution, WaitpointWaitRecord,
    WaitpointWaitStartRecord, WaitpointWaitStatus,
};

const WAITPOINT_EVENT_LOG_QUEUE_DEPTH: usize = 128;

thread_local! {
    static WAITPOINT_ID_SEQUENCE: RefCell<SequenceState> = RefCell::new(SequenceState::default());
    static WAITPOINT_WAIT_SEQUENCE: RefCell<SequenceState> = RefCell::new(SequenceState::default());
}

#[derive(Default)]
struct SequenceState {
    instance_key: String,
    next_seq: u64,
}

#[derive(Clone)]
struct DispatchKeys {
    instance_key: String,
    stable_base: String,
    trace_id: String,
    replay_of_event_id: Option<String>,
    agent: Option<String>,
}

#[derive(Default)]
struct CreateOptions {
    id: Option<String>,
    by: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Default)]
struct SignalOptions {
    by: Option<String>,
    reason: Option<String>,
}

#[derive(Default)]
struct WaitOptions {
    timeout: Option<StdDuration>,
    wait_id: Option<String>,
}

pub(crate) fn register_waitpoint_builtins(vm: &mut Vm) {
    vm.register_async_builtin("waitpoint_create", |args| async move {
        waitpoint_create_impl(&args).await
    });
    vm.register_async_builtin("waitpoint_complete", |args| async move {
        waitpoint_complete_impl(&args).await
    });
    vm.register_async_builtin("waitpoint_cancel", |args| async move {
        waitpoint_cancel_impl(&args).await
    });
    vm.register_async_builtin("waitpoint_wait", |args| async move {
        waitpoint_wait_impl(&args).await
    });
}

pub(crate) fn reset_waitpoint_state() {
    WAITPOINT_ID_SEQUENCE.with(|slot| {
        *slot.borrow_mut() = SequenceState::default();
    });
    WAITPOINT_WAIT_SEQUENCE.with(|slot| {
        *slot.borrow_mut() = SequenceState::default();
    });
}

async fn waitpoint_create_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let options = parse_create_options(args.first(), "waitpoint_create")?;
    let dispatch_keys = current_dispatch_keys();
    let id = options
        .id
        .unwrap_or_else(|| next_waitpoint_id(dispatch_keys.as_ref()));
    let by = default_actor(options.by, dispatch_keys.as_ref());
    let record = create_waitpoint(&ensure_waitpoint_event_log(), &id, by, options.metadata)
        .await
        .map_err(log_error)?;
    value_from_serde(&record)
}

async fn waitpoint_complete_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let id = required_waitpoint_id(args.first(), "waitpoint_complete")?;
    let options = parse_signal_options(args.get(1), "waitpoint_complete")?;
    let record = complete_waitpoint(
        &ensure_waitpoint_event_log(),
        &id,
        default_actor(options.by, current_dispatch_keys().as_ref()),
    )
    .await
    .map_err(log_error)?;
    value_from_serde(&record)
}

async fn waitpoint_cancel_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let id = required_waitpoint_id(args.first(), "waitpoint_cancel")?;
    let options = parse_signal_options(args.get(1), "waitpoint_cancel")?;
    let record = cancel_waitpoint(
        &ensure_waitpoint_event_log(),
        &id,
        default_actor(options.by, current_dispatch_keys().as_ref()),
        options.reason,
    )
    .await
    .map_err(log_error)?;
    value_from_serde(&record)
}

async fn waitpoint_wait_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let ids = waitpoint_ids_from_value(args.first(), "waitpoint_wait")?;
    let ids = dedupe_waitpoint_ids(&ids);
    if ids.is_empty() {
        return Err(VmError::Runtime(
            "waitpoint_wait: expected at least one waitpoint id".to_string(),
        ));
    }

    let options = parse_wait_options(args.get(1), "waitpoint_wait")?;
    let dispatch_keys = current_dispatch_keys();
    let wait_id = options
        .wait_id
        .unwrap_or_else(|| next_wait_id(dispatch_keys.as_ref()));
    let log = ensure_waitpoint_event_log();

    if let Some(record) = find_wait_terminal(&log, &wait_id)
        .await
        .map_err(log_error)?
    {
        return wait_record_to_value(record);
    }
    if is_replay() {
        return Err(VmError::Runtime(format!(
            "replay is missing a recorded waitpoint result for '{wait_id}'"
        )));
    }

    let start_record = WaitpointWaitStartRecord {
        wait_id: wait_id.clone(),
        waitpoint_ids: ids.clone(),
        started_at: now_rfc3339(),
        trace_id: Some(trace_id(dispatch_keys.as_ref())),
        replay_of_event_id: dispatch_keys
            .as_ref()
            .and_then(|keys| keys.replay_of_event_id.clone()),
    };
    append_wait_started(&log, &start_record)
        .await
        .map_err(log_error)?;

    let wait_lease = current_dispatch_wait_lease();
    if let Some(lease) = wait_lease.as_ref() {
        lease.suspend().await.map_err(dispatch_error)?;
    }

    let wait_result =
        wait_for_waitpoints_live(&log, &wait_id, &ids, &start_record, options.timeout).await;
    let resume_result = async {
        if let Some(lease) = wait_lease.as_ref() {
            lease.resume().await.map_err(dispatch_error)?;
        }
        Ok::<(), VmError>(())
    }
    .await;

    let record = wait_result?;
    append_wait_terminal(&log, &record)
        .await
        .map_err(log_error)?;
    resume_result?;
    wait_record_to_value(record)
}

async fn wait_for_waitpoints_live(
    log: &std::sync::Arc<AnyEventLog>,
    wait_id: &str,
    ids: &[String],
    start: &WaitpointWaitStartRecord,
    timeout: Option<StdDuration>,
) -> Result<WaitpointWaitRecord, VmError> {
    let mut streams = SelectAll::new();
    for id in ids {
        let topic = waitpoint_topic(id).map_err(log_error)?;
        let from = log.latest(&topic).await.map_err(log_error)?;
        let stream = log
            .clone()
            .subscribe(&topic, from)
            .await
            .map_err(log_error)?;
        streams.push(stream);
    }
    pin_mut!(streams);

    let vm = clone_async_builtin_child_vm();
    let mut poll = tokio::time::interval(StdDuration::from_millis(10));
    let deadline = timeout.map(|timeout| tokio::time::Instant::now() + timeout);

    loop {
        let waitpoints = load_waitpoints(log, ids).await.map_err(log_error)?;
        match resolve_waitpoints(ids, &waitpoints) {
            WaitpointResolution::Completed => {
                return Ok(build_wait_record(
                    wait_id,
                    ids,
                    start,
                    WaitpointWaitStatus::Completed,
                    waitpoints,
                    None,
                    None,
                ));
            }
            WaitpointResolution::Cancelled { waitpoint_id } => {
                return Ok(build_wait_record(
                    wait_id,
                    ids,
                    start,
                    WaitpointWaitStatus::Cancelled,
                    waitpoints,
                    Some(waitpoint_id),
                    None,
                ));
            }
            WaitpointResolution::Pending => {}
        }

        tokio::select! {
            maybe_event = streams.next() => {
                let Some(next) = maybe_event else {
                    return Err(VmError::Runtime(format!(
                        "waitpoint_wait: wait for '{wait_id}' ended before a terminal state was observed"
                    )));
                };
                next.map_err(log_error)?;
            }
            _ = poll.tick() => {
                if vm.as_ref().is_some_and(|vm| vm.is_cancel_requested()) {
                    return Ok(build_wait_record(
                        wait_id,
                        ids,
                        start,
                        WaitpointWaitStatus::Interrupted,
                        load_waitpoints(log, ids).await.map_err(log_error)?,
                        None,
                        Some("VM cancelled by host".to_string()),
                    ));
                }
                if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                    return Ok(build_wait_record(
                        wait_id,
                        ids,
                        start,
                        WaitpointWaitStatus::TimedOut,
                        load_waitpoints(log, ids).await.map_err(log_error)?,
                        None,
                        Some("deadline elapsed".to_string()),
                    ));
                }
            }
        }
    }
}

fn build_wait_record(
    wait_id: &str,
    ids: &[String],
    start: &WaitpointWaitStartRecord,
    status: WaitpointWaitStatus,
    waitpoints: Vec<WaitpointRecord>,
    cancelled_waitpoint_id: Option<String>,
    reason: Option<String>,
) -> WaitpointWaitRecord {
    WaitpointWaitRecord {
        wait_id: wait_id.to_string(),
        waitpoint_ids: ids.to_vec(),
        status,
        started_at: start.started_at.clone(),
        resolved_at: now_rfc3339(),
        waitpoints,
        cancelled_waitpoint_id,
        trace_id: start.trace_id.clone(),
        replay_of_event_id: start.replay_of_event_id.clone(),
        reason,
    }
}

fn wait_record_to_value(record: WaitpointWaitRecord) -> Result<VmValue, VmError> {
    if record.status == WaitpointWaitStatus::Interrupted {
        return Err(cancelled_vm_error());
    }
    value_from_serde(&record)
}

fn required_waitpoint_id(value: Option<&VmValue>, builtin: &str) -> Result<String, VmError> {
    waitpoint_ids_from_value(value, builtin)?
        .into_iter()
        .next()
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected a waitpoint id")))
}

fn waitpoint_ids_from_value(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: expected a waitpoint id, handle, or list"
        )));
    };
    match value {
        VmValue::String(text) => Ok(vec![text.to_string()]),
        VmValue::Dict(map) => {
            let id = string_field(map, "id")?.ok_or_else(|| {
                VmError::Runtime(format!("{builtin}: waitpoint handle must include id"))
            })?;
            Ok(vec![id])
        }
        VmValue::List(items) => {
            let mut out = Vec::new();
            for item in items.iter() {
                out.extend(waitpoint_ids_from_value(Some(item), builtin)?);
            }
            Ok(out)
        }
        other => Err(VmError::Runtime(format!(
            "{builtin}: unsupported waitpoint target {}",
            other.type_name()
        ))),
    }
}

fn parse_create_options(value: Option<&VmValue>, builtin: &str) -> Result<CreateOptions, VmError> {
    let Some(value) = value else {
        return Ok(CreateOptions::default());
    };
    match value {
        VmValue::String(text) => Ok(CreateOptions {
            id: Some(text.to_string()),
            ..CreateOptions::default()
        }),
        VmValue::Dict(map) => Ok(CreateOptions {
            id: string_field(map, "id")?,
            by: string_field(map, "by")?,
            metadata: json_dict_field(map, "metadata", builtin)?,
        }),
        other => Err(VmError::Runtime(format!(
            "{builtin}: expected string or dict, got {}",
            other.type_name()
        ))),
    }
}

fn parse_signal_options(value: Option<&VmValue>, builtin: &str) -> Result<SignalOptions, VmError> {
    let Some(value) = value else {
        return Ok(SignalOptions::default());
    };
    match value {
        VmValue::Dict(map) => Ok(SignalOptions {
            by: string_field(map, "by")?,
            reason: string_field(map, "reason")?,
        }),
        other => Err(VmError::Runtime(format!(
            "{builtin}: expected dict options, got {}",
            other.type_name()
        ))),
    }
}

fn parse_wait_options(value: Option<&VmValue>, builtin: &str) -> Result<WaitOptions, VmError> {
    let Some(value) = value else {
        return Ok(WaitOptions::default());
    };
    let VmValue::Dict(map) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: expected dict options, got {}",
            value.type_name()
        )));
    };
    let timeout = match map.get("timeout") {
        Some(value) => Some(parse_duration_value(value)?),
        None => None,
    };
    Ok(WaitOptions {
        timeout,
        wait_id: string_field(map, "wait_id")?,
    })
}

fn json_dict_field(
    map: &Rc<BTreeMap<String, VmValue>>,
    field: &str,
    builtin: &str,
) -> Result<BTreeMap<String, serde_json::Value>, VmError> {
    let Some(value) = map.get(field) else {
        return Ok(BTreeMap::new());
    };
    let VmValue::Dict(entries) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: {field} must be a dict"
        )));
    };
    Ok(entries
        .iter()
        .map(|(key, value)| (key.clone(), vm_value_to_json(value)))
        .collect())
}

fn string_field(
    map: &Rc<BTreeMap<String, VmValue>>,
    field: &str,
) -> Result<Option<String>, VmError> {
    let Some(value) = map.get(field) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(text) if text.trim().is_empty() => Ok(None),
        VmValue::String(text) => Ok(Some(text.to_string())),
        other => Err(VmError::Runtime(format!(
            "{field}: expected string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_duration_value(value: &VmValue) -> Result<StdDuration, VmError> {
    match value {
        VmValue::Duration(ms) => Ok(StdDuration::from_millis(*ms)),
        VmValue::Int(ms) if *ms >= 0 => Ok(StdDuration::from_millis(*ms as u64)),
        VmValue::Float(ms) if *ms >= 0.0 => Ok(StdDuration::from_millis(*ms as u64)),
        _ => Err(VmError::Runtime(
            "waitpoint_wait: expected timeout duration or millisecond count".to_string(),
        )),
    }
}

fn default_actor(explicit: Option<String>, dispatch_keys: Option<&DispatchKeys>) -> Option<String> {
    explicit
        .filter(|value| !value.trim().is_empty())
        .or_else(|| dispatch_keys.and_then(|keys| keys.agent.clone()))
        .or_else(|| Some("system".to_string()))
}

fn current_dispatch_keys() -> Option<DispatchKeys> {
    let context = current_dispatch_context()?;
    let stable_base = context
        .replay_of_event_id
        .clone()
        .unwrap_or_else(|| context.trigger_event.id.0.clone());
    let instance_key = format!(
        "{}::{}",
        context.trigger_event.id.0,
        context.replay_of_event_id.as_deref().unwrap_or("live")
    );
    Some(DispatchKeys {
        instance_key,
        stable_base,
        trace_id: context.trigger_event.trace_id.0,
        replay_of_event_id: context.replay_of_event_id,
        agent: Some(context.agent_id),
    })
}

fn next_waitpoint_id(dispatch_keys: Option<&DispatchKeys>) -> String {
    if let Some(keys) = dispatch_keys {
        let seq = WAITPOINT_ID_SEQUENCE.with(|slot| {
            let mut state = slot.borrow_mut();
            if state.instance_key != keys.instance_key {
                state.instance_key = keys.instance_key.clone();
                state.next_seq = 0;
            }
            state.next_seq += 1;
            state.next_seq
        });
        return format!("waitpoint_{}_{}", keys.stable_base, seq);
    }
    format!("waitpoint_{}", Uuid::now_v7())
}

fn next_wait_id(dispatch_keys: Option<&DispatchKeys>) -> String {
    if let Some(keys) = dispatch_keys {
        let seq = WAITPOINT_WAIT_SEQUENCE.with(|slot| {
            let mut state = slot.borrow_mut();
            if state.instance_key != keys.instance_key {
                state.instance_key = keys.instance_key.clone();
                state.next_seq = 0;
            }
            state.next_seq += 1;
            state.next_seq
        });
        return format!("waitpoint_wait_{}_{}", keys.stable_base, seq);
    }
    format!("waitpoint_wait_{}", Uuid::now_v7())
}

fn trace_id(dispatch_keys: Option<&DispatchKeys>) -> String {
    dispatch_keys
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(|| format!("trace_{}", Uuid::now_v7()))
}

fn ensure_waitpoint_event_log() -> std::sync::Arc<AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(WAITPOINT_EVENT_LOG_QUEUE_DEPTH))
}

fn cancelled_vm_error() -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(
        "kind:cancelled:VM cancelled by host",
    )))
}

fn is_replay() -> bool {
    std::env::var("HARN_REPLAY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty() && value != "0")
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().to_string())
}

fn log_error(error: impl std::fmt::Display) -> VmError {
    VmError::Runtime(error.to_string())
}

fn dispatch_error(error: impl std::fmt::Display) -> VmError {
    VmError::Runtime(format!("waitpoint dispatcher integration: {error}"))
}

fn value_from_serde<T: Serialize>(value: &T) -> Result<VmValue, VmError> {
    serde_json::to_value(value)
        .map_err(|error| VmError::Runtime(error.to_string()))
        .map(|value| crate::stdlib::json_to_vm_value(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
    use crate::waitpoints::WAITPOINT_WAITS_TOPIC;
    use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm};

    async fn execute_waitpoint_script(
        base_dir: &std::path::Path,
        source: &str,
    ) -> Result<(String, Vec<String>), VmError> {
        reset_thread_local_state();
        let log = install_default_for_base_dir(base_dir).expect("install event log");
        let chunk = compile_source(source).expect("compile source");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.set_source_dir(base_dir);
        vm.execute(&chunk).await?;
        let output = vm.output().trim_end().to_string();
        let waits = log
            .read_range(
                &Topic::new(WAITPOINT_WAITS_TOPIC).expect("valid waitpoint waits topic"),
                None,
                usize::MAX,
            )
            .await
            .expect("read waitpoint waits")
            .into_iter()
            .map(|(_, event)| event.kind)
            .collect();
        Ok((output, waits))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waitpoint_wait_replays_from_recorded_terminal_result() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  waitpoint_create("demo")
  let completer = spawn {
    sleep(20ms)
    waitpoint_complete("demo")
  }
  let result = waitpoint_wait("demo", {wait_id: "wait-demo"})
  await(completer)
  println(result.status)
  println(result.waitpoints[0].completed_by)
}
"#;

                let (output, wait_events) = execute_waitpoint_script(dir.path(), source)
                    .await
                    .expect("live waitpoint script succeeds");
                assert_eq!(output, "completed\nsystem");
                assert_eq!(
                    wait_events,
                    vec![
                        "waitpoint_wait_started".to_string(),
                        "waitpoint_wait_completed".to_string(),
                    ]
                );

                std::env::set_var("HARN_REPLAY", "1");
                let replay = execute_waitpoint_script(dir.path(), source)
                    .await
                    .expect("replay waitpoint script succeeds");
                std::env::remove_var("HARN_REPLAY");

                assert_eq!(replay.0, output);
                assert_eq!(replay.1, wait_events);
            })
            .await;
    }
}
