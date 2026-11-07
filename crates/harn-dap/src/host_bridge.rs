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
//! `dispatch` is async. It writes the reverse request to a shared stdout
//! and awaits a `tokio::sync::oneshot` keyed by request seq, so an async
//! caller on the DAP LocalSet is not blocked on a sync `recv`. A
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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use harn_vm::json_to_vm_value;
use harn_vm::llm::vm_value_dict_to_json;
use harn_vm::{HostCallBridge, HostCallDispatchFuture, VmError, VmValue};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::framing::{write_json_frame, SharedWriter};
use crate::protocol::DapResponse;

/// Default reverse-request timeout. Generous because the client may need
/// to do real work (LSP queries, file scans) before responding.
const REVERSE_REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// Inner state of the pending-request map: the map itself plus a condvar
/// the bridge notifies whenever it registers a new pending request.
/// Downstream consumers (DAP reply router, tests) use the condvar to wake
/// deterministically instead of polling.
#[derive(Debug, Default)]
pub struct PendingState {
    pub map: Mutex<BTreeMap<i64, oneshot::Sender<DapHostCallReply>>>,
    pub inserted: Condvar,
}

/// Shared map from outgoing reverse-request seq → response channel.
pub type PendingMap = Arc<PendingState>;

struct PendingCall {
    pending: PendingMap,
    seq: i64,
    output_seq: Arc<AtomicI64>,
    stdout: SharedWriter,
}

impl Drop for PendingCall {
    fn drop(&mut self) {
        let abandoned = self
            .pending
            .map
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.seq)
            .is_some();
        if abandoned {
            let event = json!({
                "seq": self.output_seq.fetch_add(1, Ordering::SeqCst),
                "type": "event",
                "event": "harnHostCallCancelled",
                "body": {"request_seq": self.seq},
            });
            let _ = write_json_frame(&self.stdout, &event);
        }
    }
}

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
    stdout: SharedWriter,
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
    pub fn new(seq: Arc<AtomicI64>, stdout: SharedWriter, pending: PendingMap) -> Self {
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
        let _ = write_json_frame(&self.stdout, &event);
    }

    fn send_reverse_request(
        &self,
        capability: &str,
        operation: &str,
        params_json: Value,
    ) -> Result<(PendingCall, oneshot::Receiver<DapHostCallReply>), VmError> {
        let req_seq = self.next_seq();
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self
                .pending
                .map
                .lock()
                .map_err(|_| VmError::Runtime("DapHostBridge pending map poisoned".into()))?;
            guard.insert(req_seq, tx);
        }
        let call = PendingCall {
            pending: self.pending.clone(),
            seq: req_seq,
            output_seq: self.seq.clone(),
            stdout: self.stdout.clone(),
        };
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
        write_json_frame(&self.stdout, &frame)
            .map_err(|e| VmError::Runtime(format!("DAP write: {e}")))?;

        Ok((call, rx))
    }

    async fn await_reply(
        &self,
        rx: oneshot::Receiver<DapHostCallReply>,
        capability: &str,
        operation: &str,
    ) -> Result<DapHostCallReply, VmError> {
        match tokio::time::timeout(REVERSE_REQUEST_TIMEOUT, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("harnHostCall cancelled ({capability}.{operation})"),
            )))),
            Err(_) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "harnHostCall timed out after {}s ({capability}.{operation})",
                    REVERSE_REQUEST_TIMEOUT.as_secs()
                ),
            )))),
        }
    }
}

impl HostCallBridge for DapHostBridge {
    fn prepare_command<'a>(
        &'a self,
        operation: &'a str,
        params: &'a harn_vm::value::DictMap,
        trace: &'a harn_vm::stdlib::host::trace::HostRequestTrace,
    ) -> harn_vm::stdlib::host::HostCommandPreparationFuture<'a> {
        Box::pin(async move {
            harn_vm::stdlib::host::prepare_remote_command(
                Arc::new(self.clone()),
                operation,
                params,
                trace,
            )
            .await
        })
    }

    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a harn_vm::value::DictMap,
    ) -> HostCallDispatchFuture<'a> {
        Box::pin(async move {
            let start = std::time::Instant::now();
            self.emit_output("host_call", &format!("→ {capability}.{operation}"));

            let params_json = vm_value_dict_to_json(params);
            let (_call, rx) = self.send_reverse_request(capability, operation, params_json)?;
            let reply = self.await_reply(rx, capability, operation).await?;
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
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    detail,
                ))));
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
                    Some(v) => json_to_vm_value(&v),
                    None => json_to_vm_value(&Value::Object(map)),
                },
                Some(other) => json_to_vm_value(&other),
                None => VmValue::Nil,
            };
            Ok(Some(value))
        })
    }
}

impl harn_vm::stdlib::host::PreparedCommandTransport for DapHostBridge {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        args: Value,
        _trace: &'a harn_vm::stdlib::host::trace::HostRequestTrace,
        _cleanup: bool,
    ) -> harn_vm::stdlib::host::CommandTransportFuture<'a> {
        Box::pin(async move {
            let params = json_to_vm_value(&args);
            let params = params.as_dict().ok_or_else(|| {
                VmError::Runtime("prepared process request must be an object".into())
            })?;
            let result = self
                .dispatch("process", operation, params)
                .await?
                .ok_or_else(|| VmError::Runtime("DAP host cannot bind command execution".into()))?;
            Ok(harn_vm::llm::vm_value_to_json(&result))
        })
    }
}

impl DapHostBridge {
    /// Drain every in-flight reverse request and reply with a synthetic
    /// failure carrying `reason` as the message. Called on `disconnect`
    /// / `terminate` so any DapHostBridge::dispatch future awaiting
    /// `await_reply` unwinds promptly instead of waiting on the 60s
    /// per-op timeout. The script sees a normal Harn exception that
    /// propagates up and ends the run cleanly.
    /// Cancel a single pending host_call by its reverse-request seq
    /// id. Returns `true` if a pending entry was found and signalled,
    /// `false` otherwise. Paired with the DAP `cancel` request (#108)
    /// so the IDE's Stop pill can tear down a runaway reverse call
    /// without tearing down the whole session.
    pub fn cancel_pending(&self, request_seq: i64, reason: &str) -> bool {
        let tx = {
            let mut guard = match self.pending.map.lock() {
                Ok(g) => g,
                Err(_) => return false,
            };
            guard.remove(&request_seq)
        };
        if let Some(tx) = tx {
            let _ = tx.send(DapHostCallReply {
                success: false,
                body: None,
                message: Some(format!("cancelled: {reason}")),
            });
            true
        } else {
            false
        }
    }

    pub fn cancel_all_pending(&self, reason: &str) {
        let mut guard = match self.pending.map.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let drained: Vec<(i64, oneshot::Sender<DapHostCallReply>)> =
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

// Re-export DapResponse to keep main.rs imports tidy when forwarding
// stdout writes through the same lock used by the bridge.
#[allow(dead_code)]
pub fn write_dap_response(stdout: &SharedWriter, response: &DapResponse) -> std::io::Result<()> {
    write_json_frame(stdout, response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    /// `Write` adapter that appends to a shared `Vec<u8>` so the test
    /// can read what the bridge wrote to "stdout" without downcasting
    /// through `dyn Write`.
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedBuffer {
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
        let mut reader = std::io::BufReader::new(bytes);
        let mut frames = Vec::new();
        while let Some(body) = crate::framing::read_frame(&mut reader).expect("well-formed frame") {
            frames.push(serde_json::from_slice(&body).expect("valid JSON body"));
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
        let stdout: SharedWriter = Arc::new(Mutex::new(Box::new(SharedBuffer(Arc::clone(&buf)))));
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
                .wait_timeout(guard, Duration::from_mins(1))
                .expect("pending condvar poisoned");
            assert!(
                !timeout.timed_out(),
                "bridge never registered a pending reverse request"
            );
            guard = next;
        }
    }

    /// Spawn a helper that waits for the bridge to register a pending
    /// reverse-request and then delivers `reply` for it. Returns a
    /// `JoinHandle` so the caller can `join()` after `dispatch` returns.
    fn spawn_replier(pending: PendingMap, reply: DapHostCallReply) -> thread::JoinHandle<()> {
        harn_parser::runtime_stack::spawn(move || {
            let req_seq = await_pending_seq(&pending);
            deliver_reply(&pending, req_seq, reply);
        })
    }

    fn run_async<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local.run_until(test()).await;
        });
    }

    #[test]
    fn dropping_a_reverse_request_retires_only_its_own_pending_entry() {
        run_async(|| async {
            let (bridge, buf, pending) = rig();
            let params = harn_vm::value::DictMap::new();
            let mut first = Box::pin(bridge.dispatch("process", "prepare", &params));
            let mut second = Box::pin(bridge.dispatch("workspace", "read_text", &params));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(std::future::Future::poll(first.as_mut(), &mut context).is_pending());
            assert!(std::future::Future::poll(second.as_mut(), &mut context).is_pending());
            let ids = pending
                .map
                .lock()
                .unwrap()
                .keys()
                .copied()
                .collect::<Vec<_>>();
            assert_eq!(ids.len(), 2, "both actual requests must be registered");
            drop(first);
            assert_eq!(
                pending
                    .map
                    .lock()
                    .unwrap()
                    .keys()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![ids[1]]
            );
            deliver_reply(
                &pending,
                ids[1],
                DapHostCallReply {
                    success: true,
                    body: Some(json!({"value":"retained"})),
                    message: None,
                },
            );
            assert!(
                matches!(second.await.unwrap(), Some(VmValue::String(value)) if value == "retained")
            );
            assert!(pending.map.lock().unwrap().is_empty());
            let frames = parse_lsp_frames(&buf.lock().unwrap());
            let cancelled = frames
                .iter()
                .filter(|frame| frame["event"] == "harnHostCallCancelled")
                .collect::<Vec<_>>();
            assert_eq!(
                cancelled.len(),
                1,
                "normal replies must not emit cancellation"
            );
            assert_eq!(cancelled[0]["body"]["request_seq"], ids[0]);
            assert_eq!(
                cancelled[0]["seq"],
                ids[1] + 1,
                "events share the adapter sequence allocator"
            );
        });
    }

    #[test]
    fn prepared_commands_use_the_shared_token_protocol_over_reverse_requests() {
        run_async(|| async {
            let (bridge, buf, pending) = rig();
            let helper = spawn_replier(
                pending.clone(),
                DapHostCallReply {
                    success: true,
                    body: Some(
                        json!({"value":{"status":"bound", "token":"bound-one", "invocation":{
                            "argv":["python3", "", " spaced argument "], "cwd":"/executor/workspace",
                        }}}),
                    ),
                    message: None,
                },
            );
            let params = harn_vm::value::DictMap::new();
            let trace = harn_vm::stdlib::host::trace::HostRequestTrace::new(None, &params);
            let prepared = bridge
                .prepare_command("exec", &params, &trace)
                .await
                .unwrap();
            helper.join().unwrap();
            let harn_vm::stdlib::host::HostCommandPreparation::Bound(command) = prepared else {
                panic!("the client must bind the command");
            };
            assert_eq!(
                command.invocation().argv(),
                ["python3", "", " spaced argument "]
            );
            let replies = thread::spawn(move || {
                for value in [
                    json!({"exit_code":0}),
                    json!({"status":"released", "token":"bound-one"}),
                ] {
                    let seq = await_pending_seq(&pending);
                    deliver_reply(
                        &pending,
                        seq,
                        DapHostCallReply {
                            success: true,
                            body: Some(json!({"value":value})),
                            message: None,
                        },
                    );
                }
            });
            let result = command.execute().await.unwrap();
            assert!(matches!(
                result.as_dict().unwrap().get("exit_code"),
                Some(VmValue::Int(0))
            ));
            replies.join().unwrap();
            let bytes = buf.lock().unwrap().clone();
            let requests = parse_lsp_frames(&bytes)
                .into_iter()
                .filter(|frame| frame["command"] == "harnHostCall")
                .collect::<Vec<_>>();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[0]["arguments"]["operation"], "prepare");
            assert_eq!(requests[0]["arguments"]["params"]["operation"], "exec");
            for (request, operation) in requests[1..]
                .iter()
                .zip(["execute_prepared", "release_prepared"])
            {
                assert_eq!(request["arguments"]["operation"], operation);
                assert_eq!(request["arguments"]["params"], json!({"token":"bound-one"}));
            }
        });
    }

    #[test]
    fn dispatch_emits_reverse_request_and_unwraps_value_body() {
        run_async(|| async {
            let (bridge, buf, pending) = rig();
            let helper = spawn_replier(
                Arc::clone(&pending),
                DapHostCallReply {
                    success: true,
                    body: Some(serde_json::json!({"value": "hello"})),
                    message: None,
                },
            );

            let mut params = harn_vm::value::DictMap::new();
            params.insert("path".into(), VmValue::String(arcstr::ArcStr::from("foo")));
            params.insert("binary".into(), VmValue::Bytes(Arc::new(vec![0, 255])));
            let result = bridge
                .dispatch("workspace", "read_text", &params)
                .await
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
            assert_eq!(
                frame["arguments"]["params"]["binary"],
                json!({"$bytes_b64":"AP8="})
            );
        });
    }

    #[test]
    fn dispatch_failure_throws_with_message() {
        run_async(|| async {
            let (bridge, _buf, pending) = rig();
            let helper = spawn_replier(
                Arc::clone(&pending),
                DapHostCallReply {
                    success: false,
                    body: None,
                    message: Some("not implemented".to_string()),
                },
            );

            let result = bridge
                .dispatch("workspace", "missing_op", &harn_vm::value::DictMap::new())
                .await;
            helper.join().expect("helper panicked");

            match result {
                Err(VmError::Thrown(VmValue::String(s))) => {
                    assert!(s.contains("not implemented"), "unexpected error: {s}");
                }
                other => panic!("expected Thrown('not implemented'), got {other:?}"),
            }
        });
    }

    #[test]
    fn dispatch_returns_whole_body_when_value_key_missing() {
        run_async(|| async {
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
                .dispatch("session", "active_roots", &harn_vm::value::DictMap::new())
                .await
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
        });
    }
}
