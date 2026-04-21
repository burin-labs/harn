use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration as StdDuration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, AnyEventLog, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::triggers::dispatcher::{
    current_dispatch_context, current_dispatch_is_replay, current_dispatch_wait_lease,
};
use crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC;
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::{clone_async_builtin_child_vm, Vm};

const MONITOR_EVENT_LOG_QUEUE_DEPTH: usize = 128;
pub(crate) const MONITOR_WAITS_TOPIC: &str = "monitor.waits";

thread_local! {
    static MONITOR_WAIT_SEQUENCE: RefCell<SequenceState> = RefCell::new(SequenceState::default());
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
}

struct MonitorWaitOptions {
    condition: Rc<VmClosure>,
    source: MonitorSource,
    timeout: StdDuration,
    poll_interval: StdDuration,
    wait_id: Option<String>,
}

struct MonitorSource {
    poll: Rc<VmClosure>,
    push_filter: Option<Rc<VmClosure>>,
    prefers_push: bool,
    label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MonitorWaitStatus {
    Matched,
    TimedOut,
    Interrupted,
}

impl MonitorWaitStatus {
    fn event_kind(self) -> &'static str {
        match self {
            Self::Matched => "monitor_wait_matched",
            Self::TimedOut => "monitor_wait_timed_out",
            Self::Interrupted => "monitor_wait_interrupted",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MonitorWaitStartRecord {
    wait_id: String,
    source_label: Option<String>,
    started_at: String,
    timeout_ms: u64,
    poll_interval_ms: u64,
    prefers_push: bool,
    trace_id: Option<String>,
    replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MonitorWaitRecord {
    wait_id: String,
    status: MonitorWaitStatus,
    source_label: Option<String>,
    started_at: String,
    resolved_at: String,
    state: JsonValue,
    condition_value: JsonValue,
    poll_count: u64,
    push_wake_count: u64,
    trace_id: Option<String>,
    replay_of_event_id: Option<String>,
    reason: Option<String>,
}

pub(crate) fn register_monitor_builtins(vm: &mut Vm) {
    vm.register_async_builtin("monitor_wait_for_native", |args| async move {
        monitor_wait_for_impl(&args).await
    });
}

pub(crate) fn reset_monitor_state() {
    MONITOR_WAIT_SEQUENCE.with(|slot| {
        *slot.borrow_mut() = SequenceState::default();
    });
}

async fn monitor_wait_for_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let options = parse_wait_options(args.first())?;
    let dispatch_keys = current_dispatch_keys();
    let wait_id = options
        .wait_id
        .clone()
        .unwrap_or_else(|| next_wait_id(dispatch_keys.as_ref()));
    let log = ensure_monitor_event_log();

    if let Some(record) = find_monitor_terminal(&log, &wait_id)
        .await
        .map_err(log_error)?
    {
        return monitor_record_to_value(record);
    }
    if is_replay() {
        return Err(VmError::Runtime(format!(
            "replay is missing a recorded monitor wait result for '{wait_id}'"
        )));
    }

    let start = MonitorWaitStartRecord {
        wait_id: wait_id.clone(),
        source_label: options.source.label.clone(),
        started_at: now_rfc3339(),
        timeout_ms: duration_ms(options.timeout),
        poll_interval_ms: duration_ms(options.poll_interval),
        prefers_push: options.source.prefers_push,
        trace_id: Some(trace_id(dispatch_keys.as_ref())),
        replay_of_event_id: dispatch_keys
            .as_ref()
            .and_then(|keys| keys.replay_of_event_id.clone()),
    };
    append_monitor_started(&log, &start)
        .await
        .map_err(log_error)?;

    let wait_lease = current_dispatch_wait_lease();
    if let Some(lease) = wait_lease.as_ref() {
        lease.suspend().await.map_err(dispatch_error)?;
    }

    let wait_result = wait_for_monitor_live(&log, &options, &start).await;
    let resume_result = async {
        if let Some(lease) = wait_lease.as_ref() {
            lease.resume().await.map_err(dispatch_error)?;
        }
        Ok::<(), VmError>(())
    }
    .await;

    let record = wait_result?;
    append_monitor_terminal(&log, &record)
        .await
        .map_err(log_error)?;
    resume_result?;
    monitor_record_to_value(record)
}

async fn wait_for_monitor_live(
    log: &RcOrArcEventLog,
    options: &MonitorWaitOptions,
    start: &MonitorWaitStartRecord,
) -> Result<MonitorWaitRecord, VmError> {
    let deadline = tokio::time::Instant::now() + options.timeout;
    let mut poll_count = 0_u64;
    let mut push_wake_count = 0_u64;
    let mut last_state = JsonValue::Null;
    let mut last_condition = JsonValue::Bool(false);
    let mut last_push_event = JsonValue::Null;
    let mut closure_vm = clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("monitor wait requires an async builtin VM context".to_string())
    })?;
    let mut push_stream = if options.source.prefers_push && options.source.push_filter.is_some() {
        let topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC).map_err(log_error)?;
        let from = log.latest(&topic).await.map_err(log_error)?;
        Some(
            log.clone()
                .subscribe(&topic, from)
                .await
                .map_err(log_error)?,
        )
    } else {
        None
    };

    loop {
        if closure_vm.is_cancel_requested() {
            return Ok(build_monitor_record(
                start,
                MonitorWaitStatus::Interrupted,
                last_state,
                last_condition,
                poll_count,
                push_wake_count,
                Some("VM cancelled by host".to_string()),
            ));
        }

        let poll_context = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "wait_id": start.wait_id,
            "poll_count": poll_count,
            "last_push_event": last_push_event.clone(),
        }));
        let state = call_closure(&mut closure_vm, &options.source.poll, &[poll_context]).await?;
        poll_count += 1;
        last_state = vm_value_to_json(&state);
        let condition_value = call_closure(&mut closure_vm, &options.condition, &[state]).await?;
        let matched = condition_value.is_truthy();
        last_condition = vm_value_to_json(&condition_value);
        if matched {
            return Ok(build_monitor_record(
                start,
                MonitorWaitStatus::Matched,
                last_state,
                last_condition,
                poll_count,
                push_wake_count,
                None,
            ));
        }

        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            return Ok(build_monitor_record(
                start,
                MonitorWaitStatus::TimedOut,
                last_state,
                last_condition,
                poll_count,
                push_wake_count,
                Some("deadline elapsed".to_string()),
            ));
        };
        let delay = options.poll_interval.min(remaining);
        if let Some(push_event) = wait_for_wakeup(
            &mut closure_vm,
            &mut push_stream,
            options.source.push_filter.as_ref(),
            delay,
        )
        .await?
        {
            last_push_event = push_event;
            push_wake_count += 1;
        }
    }
}

type RcOrArcEventLog = std::sync::Arc<AnyEventLog>;

async fn wait_for_wakeup(
    closure_vm: &mut Vm,
    push_stream: &mut Option<
        futures::stream::BoxStream<'static, Result<(u64, LogEvent), crate::event_log::LogError>>,
    >,
    push_filter: Option<&Rc<VmClosure>>,
    delay: StdDuration,
) -> Result<Option<JsonValue>, VmError> {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    if let Some(stream) = push_stream.as_mut() {
        tokio::select! {
            maybe_event = stream.next() => {
                let Some(event) = maybe_event else {
                    *push_stream = None;
                    return Ok(None);
                };
                let (event_id, event) = event.map_err(log_error)?;
                let push_event = serde_json::json!({
                    "event_log_id": event_id,
                    "kind": event.kind,
                    "payload": event.payload,
                    "headers": event.headers,
                    "occurred_at_ms": event.occurred_at_ms,
                });
                let Some(filter) = push_filter else {
                    return Ok(Some(push_event));
                };
                let value = crate::stdlib::json_to_vm_value(&push_event);
                let accepted = call_closure(closure_vm, filter, &[value]).await?;
                if accepted.is_truthy() {
                    Ok(Some(push_event))
                } else {
                    Ok(None)
                }
            }
            _ = &mut sleep => Ok(None),
        }
    } else {
        sleep.await;
        Ok(None)
    }
}

fn build_monitor_record(
    start: &MonitorWaitStartRecord,
    status: MonitorWaitStatus,
    state: JsonValue,
    condition_value: JsonValue,
    poll_count: u64,
    push_wake_count: u64,
    reason: Option<String>,
) -> MonitorWaitRecord {
    MonitorWaitRecord {
        wait_id: start.wait_id.clone(),
        status,
        source_label: start.source_label.clone(),
        started_at: start.started_at.clone(),
        resolved_at: now_rfc3339(),
        state,
        condition_value,
        poll_count,
        push_wake_count,
        trace_id: start.trace_id.clone(),
        replay_of_event_id: start.replay_of_event_id.clone(),
        reason,
    }
}

fn parse_wait_options(value: Option<&VmValue>) -> Result<MonitorWaitOptions, VmError> {
    let map = required_dict(value, "monitor_wait_for_native")?;
    let condition = required_closure(map, "condition", "monitor_wait_for_native")?;
    let source = parse_source(map.get("source"))?;
    let timeout = parse_required_duration(map.get("timeout"), "timeout")?;
    let poll_interval = map
        .get("poll_interval")
        .map(parse_duration_value)
        .transpose()?
        .unwrap_or_else(|| StdDuration::from_secs(10));
    if poll_interval.is_zero() {
        return Err(VmError::Runtime(
            "monitor_wait_for_native: poll_interval must be greater than zero".to_string(),
        ));
    }
    Ok(MonitorWaitOptions {
        condition,
        source,
        timeout,
        poll_interval,
        wait_id: string_field(map, "wait_id")?,
    })
}

fn parse_source(value: Option<&VmValue>) -> Result<MonitorSource, VmError> {
    match value {
        Some(VmValue::Closure(poll)) => Ok(MonitorSource {
            poll: poll.clone(),
            push_filter: None,
            prefers_push: false,
            label: None,
        }),
        Some(VmValue::Dict(map)) => Ok(MonitorSource {
            poll: required_closure(map, "poll", "monitor_wait_for_native source")?,
            push_filter: optional_closure(map, "push_filter", "monitor_wait_for_native source")?,
            prefers_push: bool_field(map, "prefers_push")?.unwrap_or(false),
            label: string_field(map, "label")?,
        }),
        Some(other) => Err(VmError::Runtime(format!(
            "monitor_wait_for_native: source must be a closure or dict, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(
            "monitor_wait_for_native: source is required".to_string(),
        )),
    }
}

fn required_dict<'a>(
    value: Option<&'a VmValue>,
    builtin: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match value {
        Some(VmValue::Dict(map)) => Ok(map),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: expected dict options, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{builtin}: options are required"))),
    }
}

fn required_closure(
    map: &BTreeMap<String, VmValue>,
    field: &str,
    builtin: &str,
) -> Result<Rc<VmClosure>, VmError> {
    optional_closure(map, field, builtin)?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: {field} must be a closure")))
}

fn optional_closure(
    map: &BTreeMap<String, VmValue>,
    field: &str,
    builtin: &str,
) -> Result<Option<Rc<VmClosure>>, VmError> {
    let Some(value) = map.get(field) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Closure(closure) => Ok(Some(closure.clone())),
        other => Err(VmError::Runtime(format!(
            "{builtin}: {field} must be a closure, got {}",
            other.type_name()
        ))),
    }
}

fn parse_required_duration(value: Option<&VmValue>, field: &str) -> Result<StdDuration, VmError> {
    let Some(value) = value else {
        return Err(VmError::Runtime(format!(
            "monitor_wait_for_native: {field} is required"
        )));
    };
    parse_duration_value(value)
}

fn parse_duration_value(value: &VmValue) -> Result<StdDuration, VmError> {
    match value {
        VmValue::Duration(ms) => Ok(StdDuration::from_millis(*ms)),
        VmValue::Int(ms) if *ms >= 0 => Ok(StdDuration::from_millis(*ms as u64)),
        VmValue::Float(ms) if *ms >= 0.0 => Ok(StdDuration::from_millis(*ms as u64)),
        other => Err(VmError::Runtime(format!(
            "monitor_wait_for_native: expected duration or non-negative millisecond count, got {}",
            other.type_name()
        ))),
    }
}

fn string_field(map: &BTreeMap<String, VmValue>, field: &str) -> Result<Option<String>, VmError> {
    let Some(value) = map.get(field) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(text) if text.trim().is_empty() => Ok(None),
        VmValue::String(text) => Ok(Some(text.to_string())),
        other => Err(VmError::Runtime(format!(
            "monitor_wait_for_native: {field} must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn bool_field(map: &BTreeMap<String, VmValue>, field: &str) -> Result<Option<bool>, VmError> {
    let Some(value) = map.get(field) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Bool(flag) => Ok(Some(*flag)),
        other => Err(VmError::Runtime(format!(
            "monitor_wait_for_native: {field} must be a bool, got {}",
            other.type_name()
        ))),
    }
}

async fn call_closure(
    vm: &mut Vm,
    closure: &Rc<VmClosure>,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    vm.call_closure_pub(closure, args, &[]).await
}

async fn append_monitor_started(
    log: &RcOrArcEventLog,
    record: &MonitorWaitStartRecord,
) -> Result<(), crate::event_log::LogError> {
    log.append(
        &monitor_waits_topic()?,
        LogEvent::new(
            "monitor_wait_started",
            serde_json::to_value(record).map_err(|error| {
                crate::event_log::LogError::Serde(format!("monitor wait encode error: {error}"))
            })?,
        )
        .with_headers(monitor_headers(
            &record.wait_id,
            record.source_label.as_deref(),
        )),
    )
    .await
    .map(|_| ())
}

async fn append_monitor_terminal(
    log: &RcOrArcEventLog,
    record: &MonitorWaitRecord,
) -> Result<(), crate::event_log::LogError> {
    log.append(
        &monitor_waits_topic()?,
        LogEvent::new(
            record.status.event_kind(),
            serde_json::to_value(record).map_err(|error| {
                crate::event_log::LogError::Serde(format!("monitor wait encode error: {error}"))
            })?,
        )
        .with_headers(monitor_headers(
            &record.wait_id,
            record.source_label.as_deref(),
        )),
    )
    .await
    .map(|_| ())
}

async fn find_monitor_terminal(
    log: &RcOrArcEventLog,
    wait_id: &str,
) -> Result<Option<MonitorWaitRecord>, crate::event_log::LogError> {
    let events = log
        .read_range(&monitor_waits_topic()?, None, usize::MAX)
        .await?;
    let mut latest = None;
    for (_, event) in events {
        if !matches!(
            event.kind.as_str(),
            "monitor_wait_matched" | "monitor_wait_timed_out" | "monitor_wait_interrupted"
        ) {
            continue;
        }
        if event.headers.get("wait_id").map(String::as_str) != Some(wait_id) {
            continue;
        }
        let Ok(record) = serde_json::from_value::<MonitorWaitRecord>(event.payload) else {
            continue;
        };
        latest = Some(record);
    }
    Ok(latest)
}

fn monitor_waits_topic() -> Result<Topic, crate::event_log::LogError> {
    Topic::new(MONITOR_WAITS_TOPIC)
}

fn monitor_headers(wait_id: &str, source_label: Option<&str>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("wait_id".to_string(), wait_id.to_string());
    if let Some(label) = source_label {
        headers.insert("source_label".to_string(), label.to_string());
    }
    headers
}

fn monitor_record_to_value(record: MonitorWaitRecord) -> Result<VmValue, VmError> {
    if record.status == MonitorWaitStatus::Interrupted {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
            "kind:cancelled:VM cancelled by host",
        ))));
    }
    serde_json::to_value(record)
        .map_err(|error| VmError::Runtime(error.to_string()))
        .map(|value| crate::stdlib::json_to_vm_value(&value))
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
    })
}

fn next_wait_id(dispatch_keys: Option<&DispatchKeys>) -> String {
    if let Some(keys) = dispatch_keys {
        let seq = MONITOR_WAIT_SEQUENCE.with(|slot| {
            let mut state = slot.borrow_mut();
            if state.instance_key != keys.instance_key {
                state.instance_key = keys.instance_key.clone();
                state.next_seq = 0;
            }
            state.next_seq += 1;
            state.next_seq
        });
        return format!("monitor_wait_{}_{}", keys.stable_base, seq);
    }
    format!("monitor_wait_{}", Uuid::now_v7())
}

fn trace_id(dispatch_keys: Option<&DispatchKeys>) -> String {
    dispatch_keys
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(|| format!("trace_{}", Uuid::now_v7()))
}

fn ensure_monitor_event_log() -> RcOrArcEventLog {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(MONITOR_EVENT_LOG_QUEUE_DEPTH))
}

fn is_replay() -> bool {
    current_dispatch_is_replay()
        || std::env::var("HARN_REPLAY")
            .ok()
            .is_some_and(|value| !value.trim().is_empty() && value != "0")
}

fn duration_ms(duration: StdDuration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
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
    VmError::Runtime(format!("monitor dispatcher integration: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{install_default_for_base_dir, EventLog};
    use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm};

    async fn execute_monitor_script(
        base_dir: &std::path::Path,
        source: &str,
    ) -> Result<(String, std::sync::Arc<AnyEventLog>), VmError> {
        reset_thread_local_state();
        let log = install_default_for_base_dir(base_dir).expect("install event log");
        let chunk = compile_source(source).expect("compile source");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.set_source_dir(base_dir);
        vm.execute(&chunk).await?;
        Ok((vm.output().trim_end().to_string(), log))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn monitor_wait_polls_until_condition_matches() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
import { wait_for } from "std/monitors"

pipeline test(task) {
  let ready_at = timestamp() * 1000 + 25
  let result = wait_for({
    wait_id: "poll-demo",
    timeout: 500ms,
    poll_interval: 10ms,
    source: {label: "poll-demo", poll: { _ ->
      return {ready: timestamp() * 1000 >= ready_at}
    }},
    condition: { state -> state.ready },
  })
  println(result.status)
  println(result.poll_count >= 2)
}
"#;
                let (output, log) = execute_monitor_script(dir.path(), source)
                    .await
                    .expect("monitor script succeeds");
                assert_eq!(output, "matched\ntrue");
                let events = log
                    .read_range(&monitor_waits_topic().unwrap(), None, usize::MAX)
                    .await
                    .expect("read monitor waits")
                    .into_iter()
                    .map(|(_, event)| event.kind)
                    .collect::<Vec<_>>();
                assert_eq!(
                    events,
                    vec![
                        "monitor_wait_started".to_string(),
                        "monitor_wait_matched".to_string()
                    ]
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn monitor_wait_replays_recorded_terminal_result() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let live = r#"
import { wait_for } from "std/monitors"

pipeline test(task) {
  let result = wait_for({
    wait_id: "replay-demo",
    timeout: 500ms,
    poll_interval: 10ms,
    source: {poll: { _ -> return {ready: true, value: 42} }},
    condition: { state -> state.ready },
  })
  println(result.status)
  println(result.state.value)
}
"#;
                let (live_output, _) = execute_monitor_script(dir.path(), live)
                    .await
                    .expect("live monitor script succeeds");

                let replay = r#"
import { wait_for } from "std/monitors"

pipeline test(task) {
  let result = wait_for({
    wait_id: "replay-demo",
    timeout: 1ms,
    poll_interval: 1ms,
    source: {poll: { _ -> return {ready: false, value: 0} }},
    condition: { state -> state.ready },
  })
  println(result.status)
  println(result.state.value)
}
"#;
                std::env::set_var("HARN_REPLAY", "1");
                let replay_result = execute_monitor_script(dir.path(), replay).await;
                std::env::remove_var("HARN_REPLAY");
                let (replay_output, _) = replay_result.expect("replay monitor script succeeds");
                assert_eq!(replay_output, live_output);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn monitor_wait_uses_push_wakeup_before_poll_interval() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                reset_thread_local_state();
                let log = install_default_for_base_dir(dir.path()).expect("install event log");
                let push_log = log.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(StdDuration::from_millis(20)).await;
                    let topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC).unwrap();
                    push_log
                        .append(
                            &topic,
                            LogEvent::new(
                                "event_ingested",
                                serde_json::json!({
                                    "event": {
                                        "provider": "github",
                                        "kind": "deployment_status"
                                    }
                                }),
                            ),
                        )
                        .await
                        .expect("append push wakeup");
                });

                let source = r#"
import { wait_for } from "std/monitors"

pipeline test(task) {
  let ready_at = timestamp() * 1000 + 10
  let result = wait_for({
    wait_id: "push-demo",
    timeout: 500ms,
    poll_interval: 1h,
    source: {
      label: "push-demo",
      prefers_push: true,
      poll: { ctx ->
        return {
          ready: timestamp() * 1000 >= ready_at && ctx.last_push_event?.payload?.event?.kind == "deployment_status",
          event_kind: ctx.last_push_event?.payload?.event?.kind,
        }
      },
      push_filter: { event -> event.payload.event.kind == "deployment_status" },
    },
    condition: { state -> state.ready },
  })
  println(result.status)
  println(result.poll_count)
  println(result.push_wake_count)
}
"#;
                let chunk = compile_source(source).expect("compile source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_source_dir(dir.path());
                vm.execute(&chunk).await.expect("monitor script succeeds");
                assert_eq!(vm.output().trim_end(), "matched\n2\n1");
            })
            .await;
    }
}
