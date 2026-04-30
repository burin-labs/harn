use std::collections::BTreeMap;
use std::rc::Rc;

use futures::StreamExt;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmStream, VmValue};
use crate::vm::Vm;

const EVENT_LOG_QUEUE_DEPTH: usize = 128;

pub(crate) fn register_event_log_builtins(vm: &mut Vm) {
    register_event_log_namespace(vm);

    vm.register_async_builtin("event_log.emit", |args| async move {
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
    });

    vm.register_async_builtin("event_log.latest", |args| async move {
        let topic = parse_topic(args.first(), "event_log.latest")?;
        let latest = ensure_event_log().latest(&topic).await.map_err(log_error)?;
        Ok(latest
            .map(|id| VmValue::Int(id as i64))
            .unwrap_or(VmValue::Nil))
    });

    vm.register_async_builtin("event_log.subscribe", |args| async move {
        let options = parse_subscribe_options(&args)?;
        let log = ensure_event_log();
        let mut events = log
            .clone()
            .subscribe(&options.topic, options.from_cursor)
            .await
            .map_err(log_error)?;
        let topic_name = options.topic.as_str().to_string();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<VmValue, VmError>>(1);

        tokio::task::spawn_local(async move {
            while let Some(next) = events.next().await {
                let value = match next {
                    Ok((event_id, event)) => Ok(event_to_value(&topic_name, event_id, event)),
                    Err(error) => Err(log_error(error)),
                };
                if tx.send(value).await.is_err() {
                    return;
                }
            }
        });

        Ok(VmValue::Stream(VmStream {
            done: Rc::new(std::cell::Cell::new(false)),
            receiver: Rc::new(tokio::sync::Mutex::new(rx)),
        }))
    });
}

fn register_event_log_namespace(vm: &mut Vm) {
    let names = ["emit", "latest", "subscribe"];
    vm.set_global(
        "event_log",
        VmValue::Dict(Rc::new(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(Rc::from("event_log")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(Rc::from(format!("event_log.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        )),
    );
}

struct SubscribeOptions {
    topic: Topic,
    from_cursor: Option<u64>,
}

fn parse_subscribe_options(args: &[VmValue]) -> Result<SubscribeOptions, VmError> {
    match args.first() {
        Some(VmValue::Dict(options)) => {
            let topic = parse_topic(options.get("topic"), "event_log.subscribe")?;
            let from_cursor = parse_cursor(
                options
                    .get("from_cursor")
                    .or_else(|| options.get("cursor"))
                    .or_else(|| options.get("from")),
                "event_log.subscribe",
            )?;
            Ok(SubscribeOptions { topic, from_cursor })
        }
        other => Ok(SubscribeOptions {
            topic: parse_topic(other, "event_log.subscribe")?,
            from_cursor: parse_cursor(args.get(1), "event_log.subscribe")?,
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
                        out.insert(key.clone(), value.to_string());
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
