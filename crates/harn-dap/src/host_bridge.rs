//! `DapHostBridge` — implements `harn_vm::HostCallBridge` by forwarding
//! `host_call` ops to the DAP client as **reverse requests**.
//!
//! ## Why
//!
//! harn-dap is a generic Harn debugger. It has no knowledge of the
//! capabilities a particular embedder (an IDE, a CLI host) wants to
//! expose to scripts via `host_call(…)`. Rather than baking those into
//! the binary, the bridge round-trips every unhandled op back to the
//! DAP client, which is the natural authority over IDE/host resources
//! (per ACP and DAP design — the client owns environment, FS, terminals,
//! user interaction).
//!
//! ## Wire shape
//!
//! Adapter → client (server-initiated request, DAP `runInTerminal`-style):
//!
//! ```json
//! {"seq": 17, "type": "request", "command": "harnHostCall",
//!  "arguments": {"capability": "workspace", "operation": "project_root",
//!                "params": {}}}
//! ```
//!
//! Client → adapter:
//!
//! ```json
//! {"seq": 18, "type": "response", "request_seq": 17, "command": "harnHostCall",
//!  "success": true, "body": {"value": "/Users/x/proj"}}
//! ```
//!
//! On `success: false`, the adapter throws `VmError::Thrown(message)` so
//! the script sees a normal Harn exception and can `try`/`catch` it.
//!
//! ## Threading
//!
//! `dispatch` is sync. It writes the reverse request to a shared stdout
//! and blocks on a `std::sync::mpsc::Receiver` keyed by request seq. A
//! dedicated stdin reader thread owned by `main.rs` deserializes
//! incoming DAP frames; responses (those with `type == "response"`) are
//! routed into the matching pending channel here. Genuine client
//! requests (`type == "request"`) are forwarded into the main message
//! queue for the debugger to handle.
//!
//! Because the VM is single-threaded and DAP framing already serializes
//! one message at a time, the only synchronization required is the
//! pending map and a single stdout mutex.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use harn_vm::{HostCallBridge, VmError, VmValue};
use serde_json::{json, Value};

use crate::protocol::DapResponse;

/// Default reverse-request timeout. Generous because the client may need
/// to do real work (LSP queries, file scans) before responding.
const REVERSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Inner state of the pending-request map: the map itself plus a condvar
/// the bridge notifies whenever it registers a new pending request.
/// Downstream consumers (DAP reply router, tests) use the condvar to wake
/// deterministically instead of polling.
#[derive(Debug, Default)]
pub struct PendingState {
    pub map: Mutex<BTreeMap<i64, Sender<DapHostCallReply>>>,
    pub inserted: Condvar,
}

/// Shared map from outgoing reverse-request seq → response channel.
pub type PendingMap = Arc<PendingState>;

/// Create an empty pending map.
pub fn pending_map_new() -> PendingMap {
    Arc::new(PendingState::default())
}

/// What the DAP client replies with after a reverse request.
#[derive(Debug)]
pub struct DapHostCallReply {
    pub success: bool,
    /// Raw body Value when success; error message string otherwise.
    pub body: Option<Value>,
    pub message: Option<String>,
}

/// A `HostCallBridge` that round-trips ops to the DAP client as DAP
/// reverse requests. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct DapHostBridge {
    /// Shared seq counter for adapter-initiated messages.
    seq: Arc<AtomicI64>,
    /// Stdout writer, locked for framing serialization.
    stdout: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Pending reverse requests, keyed by their outgoing seq.
    pending: PendingMap,
    /// Set of ops we should *always* forward to the client. When None,
    /// we forward everything (and let the embedder return `Ok(None)` for
    /// pass-through). When Some, only listed ops forward — the rest
    /// fall through to harn-vm's standalone fallbacks. Currently unused
    /// at the binary level (we forward everything); kept for future
    /// embedders that wrap us as a library.
    #[allow(dead_code)]
    forward_ops: Option<Arc<Vec<String>>>,
}

impl DapHostBridge {
    pub fn new(
        seq: Arc<AtomicI64>,
        stdout: Arc<Mutex<Box<dyn Write + Send>>>,
        pending: PendingMap,
    ) -> Self {
        Self {
            seq,
            stdout,
            pending,
            forward_ops: None,
        }
    }

    fn next_seq(&self) -> i64 {
        // Same counter the debugger uses, so seq stays globally unique.
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Emit a DAP `output` event so the IDE's debug console / log view
    /// can show host_call activity in real time. Category `"host_call"`
    /// surfaces as `dap.host_call` in HarnLogsView.
    fn emit_output(&self, category: &str, text: &str) {
        let seq = self.next_seq();
        let event = json!({
            "seq": seq,
            "type": "event",
            "event": "output",
            "body": {
                "category": category,
                "output": format!("{text}\n"),
            }
        });
        if let Ok(body) = serde_json::to_string(&event) {
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            if let Ok(mut guard) = self.stdout.lock() {
                let _ = guard.write_all(header.as_bytes());
                let _ = guard.write_all(body.as_bytes());
                let _ = guard.flush();
            }
        }
    }

    fn send_reverse_request(
        &self,
        capability: &str,
        operation: &str,
        params_json: Value,
    ) -> Result<Receiver<DapHostCallReply>, VmError> {
        let req_seq = self.next_seq();
        let (tx, rx) = channel();
        {
            let mut guard = self
                .pending
                .map
                .lock()
                .map_err(|_| VmError::Runtime("DapHostBridge pending map poisoned".into()))?;
            guard.insert(req_seq, tx);
        }
        // Notify any waiter watching for new pending entries (e.g. tests
        // that deliver synthetic replies without spinning on a poll loop).
        self.pending.inserted.notify_all();

        let frame = json!({
            "seq": req_seq,
            "type": "request",
            "command": "harnHostCall",
            "arguments": {
                "capability": capability,
                "operation": operation,
                "params": params_json,
            },
        });
        let body = serde_json::to_string(&frame)
            .map_err(|e| VmError::Runtime(format!("DAP encode: {e}")))?;

        let mut stdout = self
            .stdout
            .lock()
            .map_err(|_| VmError::Runtime("DapHostBridge stdout mutex poisoned".into()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        stdout
            .write_all(header.as_bytes())
            .and_then(|_| stdout.write_all(body.as_bytes()))
            .and_then(|_| stdout.flush())
            .map_err(|e| VmError::Runtime(format!("DAP write: {e}")))?;

        Ok(rx)
    }

    fn await_reply(
        &self,
        rx: Receiver<DapHostCallReply>,
        capability: &str,
        operation: &str,
    ) -> Result<DapHostCallReply, VmError> {
        rx.recv_timeout(REVERSE_REQUEST_TIMEOUT).map_err(|_| {
            VmError::Thrown(VmValue::String(std::rc::Rc::from(format!(
                "harnHostCall timed out after {}s ({capability}.{operation})",
                REVERSE_REQUEST_TIMEOUT.as_secs()
            ))))
        })
    }
}

impl HostCallBridge for DapHostBridge {
    fn dispatch(
        &self,
        capability: &str,
        operation: &str,
        params: &BTreeMap<String, VmValue>,
    ) -> Result<Option<VmValue>, VmError> {
        let start = std::time::Instant::now();
        self.emit_output("host_call", &format!("→ {capability}.{operation}"));

        let params_json = vm_dict_to_json(params);
        let rx = self.send_reverse_request(capability, operation, params_json)?;
        let reply = self.await_reply(rx, capability, operation)?;
        let elapsed_ms = start.elapsed().as_millis();
        if !reply.success {
            // The client knows the op exists but it failed — surface as a
            // throwable so user pipelines can `try`/`catch` it.
            let detail = reply
                .message
                .or_else(|| reply.body.as_ref().map(|v| v.to_string()))
                .unwrap_or_else(|| format!("{capability}.{operation} failed"));
            self.emit_output(
                "host_call",
                &format!("✗ {capability}.{operation} ({elapsed_ms}ms): {detail}"),
            );
            return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(detail))));
        }
        self.emit_output(
            "host_call",
            &format!("← {capability}.{operation} ({elapsed_ms}ms)"),
        );
        // Convention: success body shape is `{"value": <result>}`. If the
        // client returned a body without `value`, treat the whole body as
        // the result. Empty body → Nil (matches ACP `fs/write_text_file`
        // returning {}).
        let value = match reply.body {
            Some(Value::Object(mut map)) => match map.remove("value") {
                Some(v) => json_to_vm_value(v),
                None => json_to_vm_value(Value::Object(map)),
            },
            Some(other) => json_to_vm_value(other),
            None => VmValue::Nil,
        };
        Ok(Some(value))
    }
}

impl DapHostBridge {
    /// Drain every in-flight reverse request and reply with a synthetic
    /// failure carrying `reason` as the message. Called on `disconnect`
    /// / `terminate` so any DapHostBridge::dispatch call blocking inside
    /// `await_reply` unwinds promptly instead of waiting on the 60s
    /// per-op timeout. The script sees a normal Harn exception that
    /// propagates up and ends the run cleanly.
    pub fn cancel_all_pending(&self, reason: &str) {
        let mut guard = match self.pending.map.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let drained: Vec<(i64, Sender<DapHostCallReply>)> =
            std::mem::take(&mut *guard).into_iter().collect();
        drop(guard);
        for (_seq, tx) in drained {
            let _ = tx.send(DapHostCallReply {
                success: false,
                body: None,
                message: Some(format!("cancelled: {reason}")),
            });
        }
    }
}

/// Send a reply for a single pending reverse request, then drop the
/// channel. Called by `main.rs` when an incoming `type: "response"`
/// frame matches one of our reverse-request seqs.
pub fn deliver_reply(pending: &PendingMap, request_seq: i64, reply: DapHostCallReply) {
    let tx = {
        let mut guard = match pending.map.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.remove(&request_seq)
    };
    if let Some(tx) = tx {
        let _ = tx.send(reply);
    }
}

// ── VmValue ↔ JSON conversion ─────────────────────────────────────────
//
// Local impls to avoid depending on harn-vm's internal `vm_value_to_json`
// (private to the LLM module). This is a small, stable surface that's
// straightforward to keep in sync with `VmValue`.

fn vm_dict_to_json(params: &BTreeMap<String, VmValue>) -> Value {
    let mut map = serde_json::Map::with_capacity(params.len());
    for (k, v) in params.iter() {
        map.insert(k.clone(), vm_value_to_json(v));
    }
    Value::Object(map)
}

fn vm_value_to_json(value: &VmValue) -> Value {
    match value {
        VmValue::Nil => Value::Null,
        VmValue::Bool(b) => Value::Bool(*b),
        VmValue::Int(i) => Value::Number((*i).into()),
        VmValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        VmValue::String(s) => Value::String(s.to_string()),
        VmValue::List(items) => Value::Array(items.iter().map(vm_value_to_json).collect()),
        VmValue::Dict(map) => {
            let mut obj = serde_json::Map::with_capacity(map.len());
            for (k, v) in map.iter() {
                obj.insert(k.clone(), vm_value_to_json(v));
            }
            Value::Object(obj)
        }
        // Anything else (closures, tasks, error values) we represent as
        // their display string. These are not normal host_call params.
        other => Value::String(other.display().to_string()),
    }
}

fn json_to_vm_value(value: Value) -> VmValue {
    match value {
        Value::Null => VmValue::Nil,
        Value::Bool(b) => VmValue::Bool(b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                VmValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                VmValue::Float(f)
            } else {
                VmValue::Nil
            }
        }
        Value::String(s) => VmValue::String(std::rc::Rc::from(s)),
        Value::Array(arr) => VmValue::List(std::rc::Rc::new(
            arr.into_iter().map(json_to_vm_value).collect(),
        )),
        Value::Object(obj) => {
            let map: BTreeMap<String, VmValue> = obj
                .into_iter()
                .map(|(k, v)| (k, json_to_vm_value(v)))
                .collect();
            VmValue::Dict(std::rc::Rc::new(map))
        }
    }
}

// Re-export DapResponse to keep main.rs imports tidy when forwarding
// stdout writes through the same lock used by the bridge.
#[allow(dead_code)]
pub fn write_dap_response(
    stdout: &Arc<Mutex<Box<dyn Write + Send>>>,
    response: &DapResponse,
) -> std::io::Result<()> {
    let body = serde_json::to_string(response)
        .map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut guard = stdout
        .lock()
        .map_err(|_| std::io::Error::other("stdout mutex poisoned"))?;
    guard.write_all(header.as_bytes())?;
    guard.write_all(body.as_bytes())?;
    guard.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    /// `Write` adapter that appends to a shared `Vec<u8>` so the test
    /// can read what the bridge wrote to "stdout" without downcasting
    /// through `dyn Write`.
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Parse every LSP frame in `bytes` in order. The test buffer
    /// accumulates all writes the bridge emits (debug output, the
    /// reverse request, the completion event), so tests need to pick
    /// the frame they're asserting on instead of assuming the first
    /// one.
    fn parse_lsp_frames(bytes: &[u8]) -> Vec<serde_json::Value> {
        let mut frames = Vec::new();
        let mut cursor = 0;
        while cursor < bytes.len() {
            let Some(header_end_rel) = bytes[cursor..].windows(4).position(|w| w == b"\r\n\r\n")
            else {
                break;
            };
            let header_bytes = &bytes[cursor..cursor + header_end_rel];
            let header_str = match std::str::from_utf8(header_bytes) {
                Ok(s) => s,
                Err(_) => break,
            };
            let content_length = header_str
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .expect("header has Content-Length");
            let body_start = cursor + header_end_rel + 4;
            let body_end = body_start + content_length;
            let frame: serde_json::Value =
                serde_json::from_slice(&bytes[body_start..body_end]).expect("valid JSON body");
            frames.push(frame);
            cursor = body_end;
        }
        frames
    }

    /// Return the first frame whose `command` matches `command`. Panics
    /// if no such frame exists — tests use this to pick the reverse
    /// request out of the bridge's mixed output stream.
    fn find_request_frame(bytes: &[u8], command: &str) -> serde_json::Value {
        parse_lsp_frames(bytes)
            .into_iter()
            .find(|v| {
                v.get("type") == Some(&serde_json::Value::String("request".into()))
                    && v.get("command") == Some(&serde_json::Value::String(command.into()))
            })
            .unwrap_or_else(|| panic!("no {command} request frame in buffer"))
    }

    fn rig() -> (DapHostBridge, Arc<Mutex<Vec<u8>>>, PendingMap) {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stdout: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter(Arc::clone(&buf)))));
        let pending: PendingMap = pending_map_new();
        let seq = Arc::new(AtomicI64::new(1));
        let bridge = DapHostBridge::new(seq, stdout, Arc::clone(&pending));
        (bridge, buf, pending)
    }

    /// Wait for the bridge to register a pending reverse-request id.
    /// Blocks on the pending map's condvar so there is no polling race:
    /// the waiter only wakes when `send_reverse_request` has already
    /// inserted an entry and called `notify_all`. The `Duration` is a
    /// safety cap in case the notifier thread dies before inserting.
    fn await_pending_seq(pending: &PendingMap) -> i64 {
        let mut guard = pending.map.lock().expect("pending map poisoned");
        loop {
            if let Some(&k) = guard.keys().next() {
                return k;
            }
            let (next, timeout) = pending
                .inserted
                .wait_timeout(guard, Duration::from_secs(60))
                .expect("pending condvar poisoned");
            if timeout.timed_out() {
                panic!("bridge never registered a pending reverse request");
            }
            guard = next;
        }
    }

    /// Spawn a helper that waits for the bridge to register a pending
    /// reverse-request and then delivers `reply` for it. Returns a
    /// `JoinHandle` so the caller can `join()` after `dispatch` returns.
    /// VmValue is `!Send`, so we never move VmValue across threads in
    /// these tests — only `Send` types (DapHostCallReply, scalars) do.
    fn spawn_replier(pending: PendingMap, reply: DapHostCallReply) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let req_seq = await_pending_seq(&pending);
            deliver_reply(&pending, req_seq, reply);
        })
    }

    #[test]
    fn dispatch_emits_reverse_request_and_unwraps_value_body() {
        let (bridge, buf, pending) = rig();
        let helper = spawn_replier(
            Arc::clone(&pending),
            DapHostCallReply {
                success: true,
                body: Some(serde_json::json!({"value": "hello"})),
                message: None,
            },
        );

        let mut params = BTreeMap::new();
        params.insert("path".into(), VmValue::String(std::rc::Rc::from("foo")));
        let result = bridge
            .dispatch("workspace", "read_text", &params)
            .expect("dispatch ok")
            .expect("Some");
        helper.join().expect("helper panicked");

        match result {
            VmValue::String(s) => assert_eq!(&*s, "hello"),
            other => panic!("expected String, got {other:?}"),
        }

        let bytes = buf.lock().unwrap().clone();
        let frame = find_request_frame(&bytes, "harnHostCall");
        assert_eq!(frame["arguments"]["capability"], "workspace");
        assert_eq!(frame["arguments"]["operation"], "read_text");
        assert_eq!(frame["arguments"]["params"]["path"], "foo");
    }

    #[test]
    fn dispatch_failure_throws_with_message() {
        let (bridge, _buf, pending) = rig();
        let helper = spawn_replier(
            Arc::clone(&pending),
            DapHostCallReply {
                success: false,
                body: None,
                message: Some("not implemented".to_string()),
            },
        );

        let result = bridge.dispatch("workspace", "missing_op", &BTreeMap::new());
        helper.join().expect("helper panicked");

        match result {
            Err(VmError::Thrown(VmValue::String(s))) => {
                assert!(s.contains("not implemented"), "unexpected error: {s}");
            }
            other => panic!("expected Thrown('not implemented'), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_returns_whole_body_when_value_key_missing() {
        let (bridge, _buf, pending) = rig();
        let helper = spawn_replier(
            Arc::clone(&pending),
            DapHostCallReply {
                success: true,
                body: Some(serde_json::json!({"roots": ["/tmp/a"]})),
                message: None,
            },
        );

        let result = bridge
            .dispatch("session", "active_roots", &BTreeMap::new())
            .expect("dispatch ok")
            .expect("Some");
        helper.join().expect("helper panicked");

        match result {
            VmValue::Dict(map) => {
                let roots = map.get("roots").expect("roots key");
                match roots {
                    VmValue::List(items) => assert_eq!(items.len(), 1),
                    other => panic!("expected list, got {other:?}"),
                }
            }
            other => panic!("expected Dict, got {other:?}"),
        }
    }
}
