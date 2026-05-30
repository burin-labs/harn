use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, sanitize_topic_component, AnyEventLog,
    EventId, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{value_structural_hash_key, VmError, VmValue};
use crate::vm::Vm;

const EVENT_KIND_COMPLETED: &str = "step.run.completed";
const STEP_LOG_SCHEMA: &str = "harn.step.run.v0";
const STEP_EVENT_TOPIC_PREFIX: &str = "step.run";
const STEP_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;
const READ_BATCH_LIMIT: usize = 1024;

thread_local! {
    static STEP_CALL_COUNTS: RefCell<BTreeMap<String, u64>> =
        const { RefCell::new(BTreeMap::new()) };
}

#[derive(Debug)]
struct StepRunRequest {
    key: String,
    input: VmValue,
    input_supplied: bool,
    callable: VmValue,
    options: StepOptions,
}

#[derive(Debug, Default)]
struct StepOptions {
    namespace: Option<String>,
}

#[derive(Debug, Clone)]
struct StepRecord {
    event_id: EventId,
    namespace: String,
    key: String,
    sequence: u64,
    deterministic_inputs_hash: String,
    result: serde_json::Value,
    occurred_at_ms: i64,
}

pub(crate) fn reset_durable_step_state() {
    STEP_CALL_COUNTS.with(|counts| counts.borrow_mut().clear());
}

pub(crate) fn register_durable_step_builtins(vm: &mut Vm) {
    register_step_namespace(vm);
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "step.run(...args: any) -> any",
    kind = "async",
    category = "durable_step"
)]
async fn step_run_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    step_run(args).await
}

#[harn_builtin(
    sig = "step.inspect(namespace_or_options?: string | dict | nil) -> list",
    kind = "async",
    category = "durable_step"
)]
async fn step_inspect_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    step_inspect(args).await
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&STEP_RUN_IMPL_DEF, &STEP_INSPECT_IMPL_DEF];

fn register_step_namespace(vm: &mut Vm) {
    let names = ["run", "inspect"];
    vm.set_global(
        "step",
        VmValue::Dict(Rc::new(
            std::iter::once(("_namespace".to_string(), VmValue::String(Rc::from("step"))))
                .chain(names.into_iter().map(|name| {
                    (
                        name.to_string(),
                        VmValue::BuiltinRef(Rc::from(format!("step.{name}"))),
                    )
                }))
                .collect::<BTreeMap<_, _>>(),
        )),
    );
}

async fn step_run(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let request = parse_step_run_request(args)?;
    let mut child_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("step.run: builtin requires VM execution context".to_string())
    })?;
    let namespace = request
        .options
        .namespace
        .clone()
        .unwrap_or_else(|| default_namespace(&child_vm));
    let topic = topic_for_namespace(&namespace)?;
    let sequence = next_sequence(&namespace, &request.key);
    let input_hash = input_hash(&request.input);
    let identity = step_identity(&namespace, &request.key, sequence, &input_hash);
    let log = ensure_step_event_log();

    if let Some(record) = find_step_record(&log, &topic, &namespace, &request.key, sequence).await?
    {
        return value_from_replay(record, &namespace, &request.key, sequence, &input_hash);
    }

    let call_args = call_args_for_handler(&request, &request.callable)?;
    let result = child_vm
        .call_callable_owned(&request.callable, call_args)
        .await;
    crate::vm::forward_child_output_to_parent(&child_vm.take_output());
    let result = result?;
    let result_json = vm_value_to_json(&result);

    let mut headers = BTreeMap::new();
    headers.insert("step_identity".to_string(), identity.clone());
    headers.insert(
        "step_key_hash".to_string(),
        sha256_hex(request.key.as_bytes()),
    );
    headers.insert(
        "step_deterministic_inputs_hash".to_string(),
        input_hash.clone(),
    );
    let payload = serde_json::json!({
        "schema": STEP_LOG_SCHEMA,
        "namespace": namespace,
        "key": request.key,
        "sequence": sequence,
        "deterministic_inputs_hash": input_hash,
        "input": vm_value_to_json(&request.input),
        "result": result_json,
    });
    let outcome = log
        .append_idempotent_by_header(
            &topic,
            "step_identity",
            &identity,
            LogEvent::new(EVENT_KIND_COMPLETED, payload).with_headers(headers),
        )
        .await
        .map_err(step_log_error)?;
    log.flush().await.map_err(step_log_error)?;

    if !outcome.inserted {
        if let Some(record) = parse_step_record(outcome.event_id, outcome.event) {
            return value_from_replay(record, &namespace, &request.key, sequence, &input_hash);
        }
    }
    Ok(result)
}

async fn step_inspect(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let child_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("step.inspect: builtin requires VM execution context".to_string())
    })?;
    let namespace = parse_step_inspect_namespace(args, &child_vm)?;
    let topic = topic_for_namespace(&namespace)?;
    let records = read_step_records(&ensure_step_event_log(), &topic).await?;
    Ok(VmValue::List(Rc::new(
        records
            .into_iter()
            .filter(|record| record.namespace == namespace)
            .map(|record| record_to_value(&record))
            .collect(),
    )))
}

fn parse_step_run_request(args: Vec<VmValue>) -> Result<StepRunRequest, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "step.run: expected key, optional input, handler, and optional options".to_string(),
        ));
    }
    let key = required_string(args.first(), "step.run", "key")?;
    if key.trim().is_empty() {
        return Err(VmError::Runtime(
            "step.run: key cannot be empty".to_string(),
        ));
    }

    let mut input = VmValue::Nil;
    let mut input_supplied = false;
    let callable;
    let options_index;
    let second = args.get(1).expect("len checked");
    if Vm::is_callable_value(second) {
        callable = second.clone();
        options_index = 2;
    } else {
        input = second.clone();
        input_supplied = true;
        callable = args.get(2).cloned().ok_or_else(|| {
            VmError::Runtime("step.run: handler is required after input".to_string())
        })?;
        options_index = 3;
    }
    if !Vm::is_callable_value(&callable) {
        return Err(VmError::TypeError(format!(
            "step.run: handler must be callable, got {}",
            callable.type_name()
        )));
    }
    let options = match args.get(options_index) {
        None => StepOptions::default(),
        Some(value) => parse_step_options(value, "step.run")?,
    };
    if args.len() > options_index + 1 {
        return Err(VmError::Runtime(format!(
            "step.run: expected at most {} arguments, got {}",
            options_index + 1,
            args.len()
        )));
    }

    Ok(StepRunRequest {
        key,
        input,
        input_supplied,
        callable,
        options,
    })
}

fn parse_step_options(value: &VmValue, builtin: &str) -> Result<StepOptions, VmError> {
    let dict = match value {
        VmValue::Nil => return Ok(StepOptions::default()),
        VmValue::Dict(dict) => dict,
        other => {
            return Err(VmError::TypeError(format!(
                "{builtin}: options must be a dict, got {}",
                other.type_name()
            )))
        }
    };
    reject_unknown_options(dict, builtin, &["namespace"])?;
    let namespace = match dict.get("namespace") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::String(value)) if !value.trim().is_empty() => Some(value.to_string()),
        Some(VmValue::String(_)) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: options.namespace cannot be empty"
            )))
        }
        Some(other) => {
            return Err(VmError::TypeError(format!(
                "{builtin}: options.namespace must be a string, got {}",
                other.type_name()
            )))
        }
    };
    Ok(StepOptions { namespace })
}

fn parse_step_inspect_namespace(args: Vec<VmValue>, vm: &Vm) -> Result<String, VmError> {
    if args.len() > 1 {
        return Err(VmError::Runtime(format!(
            "step.inspect: expected at most 1 argument, got {}",
            args.len()
        )));
    }
    match args.first() {
        None | Some(VmValue::Nil) => Ok(default_namespace(vm)),
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.to_string()),
        Some(VmValue::Dict(_)) => Ok(parse_step_options(args.first().unwrap(), "step.inspect")?
            .namespace
            .unwrap_or_else(|| default_namespace(vm))),
        Some(VmValue::String(_)) => Err(VmError::Runtime(
            "step.inspect: namespace cannot be empty".to_string(),
        )),
        Some(other) => Err(VmError::TypeError(format!(
            "step.inspect: expected namespace string or options dict, got {}",
            other.type_name()
        ))),
    }
}

fn reject_unknown_options(
    dict: &BTreeMap<String, VmValue>,
    builtin: &str,
    allowed: &[&str],
) -> Result<(), VmError> {
    for key in dict.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(VmError::Runtime(format!(
                "{builtin}: unknown option `{key}`"
            )));
        }
    }
    Ok(())
}

fn required_string(value: Option<&VmValue>, builtin: &str, name: &str) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{builtin}: missing {name}"))),
    }
}

fn call_args_for_handler(
    request: &StepRunRequest,
    callable: &VmValue,
) -> Result<Vec<VmValue>, VmError> {
    let VmValue::Closure(closure) = callable else {
        return Ok(if request.input_supplied {
            vec![request.input.clone()]
        } else {
            Vec::new()
        });
    };
    let required = closure.func.required_param_count();
    if required > 1 {
        return Err(VmError::Runtime(format!(
            "step.run: handler expects {required} required arguments; step handlers receive at most one input value"
        )));
    }
    if closure.func.params.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![request.input.clone()])
    }
}

fn default_namespace(vm: &Vm) -> String {
    if let Some(source_file) = vm
        .source_file
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return source_file.to_string();
    }
    if let Some(project_root) = vm.project_root() {
        return project_root.to_string_lossy().into_owned();
    }
    "default".to_string()
}

fn topic_for_namespace(namespace: &str) -> Result<Topic, VmError> {
    let safe_namespace = sanitize_topic_component(namespace);
    Topic::new(format!("{STEP_EVENT_TOPIC_PREFIX}.{safe_namespace}")).map_err(step_log_error)
}

fn ensure_step_event_log() -> Arc<AnyEventLog> {
    active_event_log().unwrap_or_else(|| install_memory_for_current_thread(STEP_LOG_QUEUE_DEPTH))
}

fn next_sequence(namespace: &str, key: &str) -> u64 {
    let counter_key = format!("{namespace}\u{0}{key}");
    STEP_CALL_COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        let next = counts.get(&counter_key).copied().unwrap_or(0) + 1;
        counts.insert(counter_key, next);
        next
    })
}

fn input_hash(input: &VmValue) -> String {
    let key = value_structural_hash_key(input);
    format!("sha256:{}", sha256_hex(key.as_bytes()))
}

fn step_identity(namespace: &str, key: &str, sequence: u64, input_hash: &str) -> String {
    let material = format!("{namespace}\u{0}{key}\u{0}{sequence}\u{0}{input_hash}");
    format!("sha256:{}", sha256_hex(material.as_bytes()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn find_step_record(
    log: &Arc<AnyEventLog>,
    topic: &Topic,
    namespace: &str,
    key: &str,
    sequence: u64,
) -> Result<Option<StepRecord>, VmError> {
    for record in read_step_records(log, topic).await? {
        if record.namespace == namespace && record.key == key && record.sequence == sequence {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

async fn read_step_records(
    log: &Arc<AnyEventLog>,
    topic: &Topic,
) -> Result<Vec<StepRecord>, VmError> {
    let mut records = Vec::new();
    let mut cursor = None;
    loop {
        let batch = log
            .read_range(topic, cursor, READ_BATCH_LIMIT)
            .await
            .map_err(step_log_error)?;
        if batch.is_empty() {
            break;
        }
        for (event_id, event) in batch {
            cursor = Some(event_id);
            if let Some(record) = parse_step_record(event_id, event) {
                records.push(record);
            }
        }
    }
    Ok(records)
}

fn parse_step_record(event_id: EventId, event: LogEvent) -> Option<StepRecord> {
    if event.kind != EVENT_KIND_COMPLETED {
        return None;
    }
    let payload = event.payload.as_object()?;
    if payload.get("schema")?.as_str()? != STEP_LOG_SCHEMA {
        return None;
    }
    Some(StepRecord {
        event_id,
        namespace: payload.get("namespace")?.as_str()?.to_string(),
        key: payload.get("key")?.as_str()?.to_string(),
        sequence: payload.get("sequence")?.as_u64()?,
        deterministic_inputs_hash: payload
            .get("deterministic_inputs_hash")?
            .as_str()?
            .to_string(),
        result: payload
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        occurred_at_ms: event.occurred_at_ms,
    })
}

fn value_from_replay(
    record: StepRecord,
    namespace: &str,
    key: &str,
    sequence: u64,
    input_hash: &str,
) -> Result<VmValue, VmError> {
    if record.deterministic_inputs_hash != input_hash {
        return Err(VmError::Runtime(format!(
            "step.run: deterministic input mismatch for key `{key}` occurrence {sequence} in namespace `{namespace}` (cached {}, current {})",
            record.deterministic_inputs_hash, input_hash
        )));
    }
    Ok(crate::stdlib::json_to_vm_value(&record.result))
}

fn record_to_value(record: &StepRecord) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "event_id": record.event_id,
        "namespace": record.namespace,
        "key": record.key,
        "sequence": record.sequence,
        "deterministic_inputs_hash": record.deterministic_inputs_hash,
        "result": record.result,
        "occurred_at_ms": record.occurred_at_ms,
    }))
}

fn step_log_error(error: crate::event_log::LogError) -> VmError {
    VmError::Runtime(format!("step: event log: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_str(value: &str) -> VmValue {
        VmValue::String(Rc::from(value))
    }

    #[test]
    fn explicit_input_hashes_are_stable_and_type_sensitive() {
        assert_eq!(input_hash(&vm_str("1")), input_hash(&vm_str("1")));
        assert_ne!(input_hash(&vm_str("1")), input_hash(&VmValue::Int(1)));
    }

    #[test]
    fn sequences_are_per_namespace_and_key() {
        reset_durable_step_state();
        assert_eq!(next_sequence("ns", "load"), 1);
        assert_eq!(next_sequence("ns", "load"), 2);
        assert_eq!(next_sequence("ns", "other"), 1);
        assert_eq!(next_sequence("other", "load"), 1);
        reset_durable_step_state();
    }

    #[test]
    fn identities_include_the_raw_namespace() {
        let hash = "sha256:abc";

        assert_ne!(
            step_identity("tenant/a", "load", 1, hash),
            step_identity("tenant_a", "load", 1, hash)
        );
    }

    #[test]
    fn options_reject_unknown_keys() {
        let options = VmValue::Dict(Rc::new(BTreeMap::from([(
            "namesapce".to_string(),
            vm_str("oops"),
        )])));

        let err = parse_step_options(&options, "step.run").unwrap_err();

        match err {
            VmError::Runtime(message) => assert!(message.contains("namesapce")),
            other => panic!("expected Runtime error, got {other:?}"),
        }
    }
}
