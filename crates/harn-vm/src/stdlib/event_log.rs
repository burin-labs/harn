use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use futures::StreamExt;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmStream, VmValue};
use crate::vm::Vm;

const EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;

pub(crate) fn register_event_log_builtins(vm: &mut Vm) {
    register_event_log_namespace(vm);
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "event_log.describe() -> dict",
    kind = "async",
    category = "event_log"
)]
async fn event_log_describe_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::TypeError(
            "event_log.describe: expected no arguments".to_string(),
        ));
    }
    let description = ensure_event_log().describe();
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "backend": description.backend.to_string(),
        "location": description
            .location
            .map(|path| path.to_string_lossy().into_owned()),
        "size_bytes": description.size_bytes,
        "queue_depth": description.queue_depth,
    })))
}

#[harn_builtin(
    sig = "event_log.emit(topic: string, kind: string, payload?: any, headers?: dict) -> int",
    kind = "async",
    category = "event_log"
)]
async fn event_log_emit_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let topic = parse_topic(args.first(), "event_log.emit")?;
    let kind = required_string(args.get(1), "event_log.emit", "kind")?;
    let payload = args
        .get(2)
        .map(vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let headers = parse_headers(args.get(3), "event_log.emit")?;
    let id = ensure_event_log()
        .append(&topic, LogEvent::new(kind, payload).with_headers(headers))
        .await
        .map_err(log_error)?;
    Ok(VmValue::Int(id as i64))
}

#[harn_builtin(
    sig = "event_log.latest(topic: string) -> int | nil",
    kind = "async",
    category = "event_log"
)]
async fn event_log_latest_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let topic = parse_topic(args.first(), "event_log.latest")?;
    let latest = ensure_event_log().latest(&topic).await.map_err(log_error)?;
    Ok(latest
        .map(|id| VmValue::Int(id as i64))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "event_log.read(topic_or_options: string | dict, from_cursor?: int | nil, limit?: int) -> list",
    kind = "async",
    category = "event_log"
)]
async fn event_log_read_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let options = parse_read_options(&args)?;
    let events = ensure_event_log()
        .read_range(&options.topic, options.from_cursor, options.limit)
        .await
        .map_err(log_error)?;
    let topic_name = options.topic.as_str().to_string();
    Ok(VmValue::List(std::sync::Arc::new(
        events
            .into_iter()
            .filter(|(_, event)| {
                options
                    .kind_prefix
                    .as_deref()
                    .is_none_or(|prefix| event.kind.starts_with(prefix))
            })
            .map(|(event_id, event)| event_to_value(&topic_name, event_id, event))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "event_log.subscribe(topic_or_options: string | dict, from_cursor?: int | nil) -> stream",
    kind = "async",
    category = "event_log"
)]
async fn event_log_subscribe_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let options = parse_subscribe_options(&args)?;
    let log = ensure_event_log();
    let mut events = log
        .clone()
        .subscribe(&options.topic, options.from_cursor)
        .await
        .map_err(log_error)?;
    let topic_name = options.topic.as_str().to_string();
    let kind_prefix = options.kind_prefix.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<VmValue, VmError>>(1);

    tokio::task::spawn_local(async move {
        while let Some(next) = events.next().await {
            let value = match next {
                Ok((event_id, event)) => {
                    if kind_prefix
                        .as_deref()
                        .is_some_and(|prefix| !event.kind.starts_with(prefix))
                    {
                        continue;
                    }
                    Ok(event_to_value(&topic_name, event_id, event))
                }
                Err(error) => Err(log_error(error)),
            };
            if tx.send(value).await.is_err() {
                return;
            }
        }
    });

    Ok(VmValue::stream(VmStream {
        done: Arc::new(AtomicBool::new(false)),
        receiver: Arc::new(tokio::sync::Mutex::new(rx)),
        cancel: None,
    }))
}

#[harn_builtin(
    sig = "event_log.topics() -> list<string>",
    kind = "async",
    category = "event_log"
)]
async fn event_log_topics_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::TypeError(
            "event_log.topics: expected no arguments".to_string(),
        ));
    }
    Ok(VmValue::List(std::sync::Arc::new(
        ensure_event_log()
            .topics()
            .await
            .map_err(log_error)?
            .into_iter()
            .map(|topic| VmValue::String(arcstr::ArcStr::from(topic.as_str())))
            .collect(),
    )))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &EVENT_LOG_DESCRIBE_IMPL_DEF,
    &EVENT_LOG_EMIT_IMPL_DEF,
    &EVENT_LOG_LATEST_IMPL_DEF,
    &EVENT_LOG_READ_IMPL_DEF,
    &EVENT_LOG_SUBSCRIBE_IMPL_DEF,
    &EVENT_LOG_TOPICS_IMPL_DEF,
];

fn register_event_log_namespace(vm: &mut Vm) {
    let names = ["describe", "emit", "latest", "read", "subscribe", "topics"];
    vm.set_global(
        "event_log",
        VmValue::dict(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(arcstr::ArcStr::from("event_log")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(format!("event_log.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        ),
    );
}

struct SubscribeOptions {
    topic: Topic,
    from_cursor: Option<u64>,
    kind_prefix: Option<String>,
}

struct ReadOptions {
    topic: Topic,
    from_cursor: Option<u64>,
    limit: usize,
    kind_prefix: Option<String>,
}

const EVENT_LOG_READ_DEFAULT_LIMIT: usize = 100;
const EVENT_LOG_READ_MAX_LIMIT: usize = 10_000;

fn parse_read_options(args: &[VmValue]) -> Result<ReadOptions, VmError> {
    match args.first() {
        Some(VmValue::Dict(options)) => {
            let topic = parse_topic(options.get("topic"), "event_log.read")?;
            let from_cursor = parse_dict_cursor(options, "event_log.read")?;
            let limit = parse_limit(options.get("limit"), "event_log.read")?;
            let kind_prefix =
                optional_string(options.get("kind_prefix"), "event_log.read", "kind_prefix")?;
            Ok(ReadOptions {
                topic,
                from_cursor,
                limit,
                kind_prefix,
            })
        }
        other => Ok(ReadOptions {
            topic: parse_topic(other, "event_log.read")?,
            from_cursor: parse_cursor(args.get(1), "event_log.read")?,
            limit: parse_limit(args.get(2), "event_log.read")?,
            kind_prefix: None,
        }),
    }
}

fn parse_subscribe_options(args: &[VmValue]) -> Result<SubscribeOptions, VmError> {
    match args.first() {
        Some(VmValue::Dict(options)) => {
            let topic = parse_topic(options.get("topic"), "event_log.subscribe")?;
            let from_cursor = parse_dict_cursor(options, "event_log.subscribe")?;
            let kind_prefix = optional_string(
                options.get("kind_prefix"),
                "event_log.subscribe",
                "kind_prefix",
            )?;
            Ok(SubscribeOptions {
                topic,
                from_cursor,
                kind_prefix,
            })
        }
        other => Ok(SubscribeOptions {
            topic: parse_topic(other, "event_log.subscribe")?,
            from_cursor: parse_cursor(args.get(1), "event_log.subscribe")?,
            kind_prefix: None,
        }),
    }
}

fn ensure_event_log() -> std::sync::Arc<crate::event_log::AnyEventLog> {
    active_event_log().unwrap_or_else(|| install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH))
}

fn parse_topic(value: Option<&VmValue>, builtin: &str) -> Result<Topic, VmError> {
    let raw = required_string(value, builtin, "topic")?;
    Topic::new(raw).map_err(log_error)
}

fn parse_cursor(value: Option<&VmValue>, builtin: &str) -> Result<Option<u64>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(Some(*n as u64)),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: from_cursor must be a non-negative int or nil, got {}",
            other.type_name()
        ))),
    }
}

/// Read the cursor from an options dict, accepting `from_cursor`, `cursor`, and
/// `from` as aliases. Shared by the `read` and `subscribe` dict forms.
fn parse_dict_cursor(
    options: &crate::value::DictMap,
    builtin: &str,
) -> Result<Option<u64>, VmError> {
    parse_cursor(
        options
            .get("from_cursor")
            .or_else(|| options.get("cursor"))
            .or_else(|| options.get("from")),
        builtin,
    )
}

fn parse_limit(value: Option<&VmValue>, builtin: &str) -> Result<usize, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(EVENT_LOG_READ_DEFAULT_LIMIT),
        Some(VmValue::Int(n)) if *n >= 0 => Ok((*n as usize).min(EVENT_LOG_READ_MAX_LIMIT)),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: limit must be a non-negative int or nil, got {}",
            other.type_name()
        ))),
    }
}

fn required_string(value: Option<&VmValue>, builtin: &str, name: &str) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!("{builtin}: missing {name}"))),
    }
}

fn optional_string(
    value: Option<&VmValue>,
    builtin: &str,
    name: &str,
) -> Result<Option<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => Ok(Some(value.to_string())),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a string or nil, got {}",
            other.type_name()
        ))),
    }
}

fn parse_headers(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<BTreeMap<String, String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(dict)) => {
            let mut out = BTreeMap::new();
            for (key, value) in dict.iter() {
                match value {
                    VmValue::String(value) => {
                        out.insert(key.to_string(), value.to_string());
                    }
                    other => {
                        return Err(VmError::TypeError(format!(
                            "{builtin}: header '{key}' must be a string, got {}",
                            other.type_name()
                        )))
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: headers must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn event_to_value(topic: &str, event_id: u64, event: LogEvent) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": event_id,
        "cursor": event_id,
        "topic": topic,
        "kind": event.kind,
        "payload": event.payload,
        "headers": event.headers,
        "occurred_at_ms": event.occurred_at_ms,
    }))
}

fn log_error(error: crate::event_log::LogError) -> VmError {
    VmError::Runtime(format!("event_log: {error}"))
}
