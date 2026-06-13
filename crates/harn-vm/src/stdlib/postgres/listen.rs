//! LISTEN/NOTIFY bridge.
//!
//! Surface (issue #2512 A.9):
//!
//! ```harn
//! // Subscribe to one or more channels; the listener handle persists
//! // across drops + reconnects until pg_listener_close is called.
//! let listener = pg_listen(db, ["receipts.updated", "captains.notice"])
//! while (true) {
//!   let notification = pg_listener_recv(listener, 5000)  // ms
//!   if (notification == nil) { continue }                // poll timeout
//!   emit_channel("pg:" + notification.channel, notification.payload)
//! }
//! pg_listener_close(listener)
//!
//! // Producer side:
//! pg_notify(db, "receipts.updated", {receipt_id: "r1"})
//! ```
//!
//! When `bridge_to_channel: true` is passed in options, the listener
//! immediately republishes each notification through `emit_channel(...)`
//! as `pg:<pg_channel>` for direct composition with the trigger DSL.
//! Production code can keep the explicit pull-loop form; the bridge is
//! sugar for one-line subscriptions.
//!
//! **Lifecycle**: listeners pin a connection from the pool for as long
//! as they're alive. Always call `pg_listener_close(listener)` — Harn
//! has no Drop semantics, so a forgotten listener leaks the connection
//! until `reset_postgres_state()` is called (typically only at VM
//! teardown). For the common request/response pattern, prefer
//! `bridge_to_channel: true` + a trigger; the dedicated pull loop is
//! only worth it for long-running consumers that already manage their
//! own lifecycle.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx_postgres::PgListener;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, TY_ANY, TY_BOOL, TY_DICT, TY_DICT_OR_NIL,
};
use crate::value::{VmError, VmValue};

use super::{handle_id, handle_value, pool_by_id, required_arg, runtime_error, HANDLE_POOL};

const HANDLE_LISTENER: &str = "pg_listener";

struct ListenerRecord {
    inner: Arc<Mutex<PgListener>>,
    bridge: bool,
    closed: Arc<AtomicBool>,
}

impl ListenerRecord {
    fn new(listener: PgListener, bridge: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(listener)),
            bridge,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
}

thread_local! {
    static LISTENERS: RefCell<BTreeMap<String, Arc<ListenerRecord>>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

pub(super) fn reset_state() {
    LISTENERS.with(|listeners| listeners.borrow_mut().clear());
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_listen", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_listen_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let pool_handle = required_arg(&args, 0, "pg_listen", "pool handle")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_listen")?;
    let pool = pool_by_id(&pool_id)?;
    let channels_value = required_arg(&args, 1, "pg_listen", "channel name(s)")?;
    let channels = parse_channel_list(channels_value)?;
    if channels.is_empty() {
        return Err(runtime_error("pg_listen: at least one channel is required"));
    }
    let options = args.get(2).and_then(VmValue::as_dict).cloned();
    let bridge = super::option_bool(
        options
            .as_ref()
            .and_then(|opts| opts.get("bridge_to_channel")),
    )
    .unwrap_or(false);

    let mut listener = PgListener::connect_with(pool.as_ref())
        .await
        .map_err(|error| runtime_error(format!("pg_listen: connect failed: {error}")))?;
    listener.ignore_pool_close_event(false);
    let refs: Vec<&str> = channels.iter().map(String::as_str).collect();
    listener
        .listen_all(refs)
        .await
        .map_err(|error| runtime_error(format!("pg_listen: LISTEN failed: {error}")))?;

    let record = Arc::new(ListenerRecord::new(listener, bridge));
    let id = super::next_id("pglisten");
    LISTENERS.with(|listeners| {
        listeners
            .borrow_mut()
            .insert(id.clone(), Arc::clone(&record));
    });

    let mut meta = crate::value::DictMap::new();
    meta.insert(
        "channels".to_string(),
        VmValue::List(std::sync::Arc::new(
            channels
                .iter()
                .map(|c| VmValue::String(std::sync::Arc::from(c.as_str())))
                .collect(),
        )),
    );
    meta.insert("bridge".to_string(), VmValue::Bool(bridge));
    Ok(handle_value(HANDLE_LISTENER, &id, meta))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_listener_recv",
        &[Param::new("args", TY_ANY)],
        TY_DICT_OR_NIL,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_listener_recv_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let handle = required_arg(&args, 0, "pg_listener_recv", "listener handle")?;
    let id = handle_id(Some(handle), HANDLE_LISTENER, "pg_listener_recv")?;
    let record = listener_by_id(&id)?;
    if record.closed.load(Ordering::Acquire) {
        return Err(runtime_error("pg_listener_recv: listener is closed"));
    }
    let timeout_ms = args.get(1).and_then(|v| match v {
        VmValue::Int(n) if *n >= 0 => Some(*n as u64),
        VmValue::Duration(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    });

    let mut listener = record.inner.lock().await;
    let notification = match timeout_ms {
        Some(ms) => match timeout(Duration::from_millis(ms), listener.recv()).await {
            Ok(result) => {
                result.map_err(|error| runtime_error(format!("pg_listener_recv: {error}")))?
            }
            Err(_) => return Ok(VmValue::Nil),
        },
        None => match listener.try_recv().await {
            Ok(Some(notification)) => notification,
            Ok(None) => return Ok(VmValue::Nil),
            Err(error) => return Err(runtime_error(format!("pg_listener_recv: {error}"))),
        },
    };

    let channel = notification.channel().to_string();
    let payload = notification.payload().to_string();
    let mut dict = crate::value::DictMap::new();
    dict.put_str("channel", channel.clone());
    dict.put_str("payload", payload.clone());
    dict.insert(
        "process_id".to_string(),
        VmValue::Int(i64::from(notification.process_id())),
    );
    let value = VmValue::dict(dict);

    if record.bridge {
        let parsed_payload: serde_json::Value = serde_json::from_str(&payload)
            .unwrap_or_else(|_| serde_json::Value::String(payload.clone()));
        let mut emit_args = Vec::with_capacity(2);
        emit_args.push(VmValue::String(std::sync::Arc::from(format!(
            "pg:{channel}"
        ))));
        emit_args.push(crate::stdlib::json_to_vm_value(&parsed_payload));
        let mut child = ctx.child_vm();
        let _ = child.call_named_builtin("emit_channel", emit_args).await;
        ctx.forward_output(&child.take_output());
    }
    Ok(value)
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_listener_close",
        &[Param::new("args", TY_ANY)],
        TY_BOOL,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_listener_close_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let handle = required_arg(&args, 0, "pg_listener_close", "listener handle")?;
    let id = handle_id(Some(handle), HANDLE_LISTENER, "pg_listener_close")?;
    let removed = LISTENERS.with(|listeners| listeners.borrow_mut().remove(&id));
    let Some(record) = removed else {
        return Ok(VmValue::Bool(false));
    };
    record.closed.store(true, Ordering::Release);
    if let Ok(mut listener) = record.inner.try_lock() {
        // Best-effort: tell PG to stop forwarding. Even on failure, the
        // listener is dropped at the end of this fn which closes the
        // underlying connection.
        let _ = listener.unlisten_all().await;
    }
    Ok(VmValue::Bool(true))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_notify", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_notify_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = required_arg(&args, 0, "pg_notify", "pool or transaction handle")?;
    let channel = required_arg(&args, 1, "pg_notify", "channel name")?;
    let channel = match channel {
        VmValue::String(text) => text.to_string(),
        _ => return Err(runtime_error("pg_notify: channel must be a string")),
    };
    validate_channel_name(&channel, "pg_notify")?;
    let payload_value = args
        .get(2)
        .cloned()
        .unwrap_or(VmValue::String(std::sync::Arc::from("")));
    let payload = match &payload_value {
        VmValue::Nil => String::new(),
        VmValue::String(text) => text.to_string(),
        other => serde_json::to_string(&crate::llm::vm_value_to_json(other)).map_err(|error| {
            runtime_error(format!("pg_notify: payload serialization failed: {error}"))
        })?,
    };

    // pg_notify(text, text) is the safe (binds-the-payload) variant —
    // NOTIFY itself only accepts a string literal which forces SQL
    // synthesis. Channel names go in the first bind position too.
    let sql = "SELECT pg_notify($1, $2)";
    let params = [
        VmValue::String(std::sync::Arc::from(channel)),
        VmValue::String(std::sync::Arc::from(payload)),
    ];
    super::execute_stmt(target, sql, &params).await?;
    Ok(VmValue::Bool(true))
}

fn parse_channel_list(value: &VmValue) -> Result<Vec<String>, VmError> {
    let mut channels = Vec::new();
    match value {
        VmValue::String(text) => channels.push(text.to_string()),
        VmValue::List(items) => {
            for item in items.as_ref() {
                let name = match item {
                    VmValue::String(text) => text.to_string(),
                    _ => {
                        return Err(runtime_error(
                            "pg_listen: channel list entries must be strings",
                        ))
                    }
                };
                channels.push(name);
            }
        }
        _ => {
            return Err(runtime_error(
                "pg_listen: channel must be a string or list of strings",
            ))
        }
    }
    for channel in &channels {
        validate_channel_name(channel, "pg_listen")?;
    }
    channels.sort();
    channels.dedup();
    Ok(channels)
}

fn validate_channel_name(name: &str, builtin: &'static str) -> Result<(), VmError> {
    // Channel names allow `.`, `:`, `-` so callers can scope per-tenant
    // (`pg:tenant-a:receipts.updated`) without quoting hassles. Postgres
    // double-quotes the channel name in the synthesized LISTEN/NOTIFY
    // anyway, but we keep the validator strict to catch typos and reject
    // SQL injection vectors at the harness boundary.
    super::validate_pg_identifier(name, builtin, "channel name", &['.', ':', '-'])
}

fn listener_by_id(id: &str) -> Result<Arc<ListenerRecord>, VmError> {
    LISTENERS.with(|listeners| {
        listeners
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| runtime_error(format!("pg_listener: unknown listener `{id}`")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name_validation() {
        assert!(validate_channel_name("receipts.updated", "pg_listen").is_ok());
        assert!(validate_channel_name("captain:notice", "pg_listen").is_ok());
        assert!(validate_channel_name("evt-123", "pg_listen").is_ok());
        assert!(validate_channel_name("", "pg_listen").is_err());
        assert!(validate_channel_name("1bad", "pg_listen").is_err());
        assert!(validate_channel_name("bad name", "pg_listen").is_err());
        assert!(validate_channel_name("bad;name", "pg_listen").is_err());
    }

    #[test]
    fn parse_channel_list_accepts_string_or_list() {
        let one = parse_channel_list(&VmValue::String(std::sync::Arc::from("foo"))).unwrap();
        assert_eq!(one, vec!["foo"]);
        let many = parse_channel_list(&VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(std::sync::Arc::from("foo")),
            VmValue::String(std::sync::Arc::from("bar")),
            VmValue::String(std::sync::Arc::from("foo")),
        ])))
        .unwrap();
        assert_eq!(many, vec!["bar", "foo"]);
        assert!(parse_channel_list(&VmValue::Int(1)).is_err());
    }

    #[test]
    fn listener_handle_is_namespaced() {
        assert_eq!(HANDLE_LISTENER, "pg_listener");
        assert_ne!(HANDLE_LISTENER, super::HANDLE_POOL);
    }
}
