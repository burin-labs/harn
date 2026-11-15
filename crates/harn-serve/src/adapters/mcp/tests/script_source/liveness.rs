//! Script handlers retain state while independent calls can suspend and cancel.
use super::super::http_disconnect::{http_call, GateConfigurator};
use super::*;
use crate::adapters::mcp::transport::spawn_http_stream_with_prelude;
use std::collections::BTreeSet;
use std::task::Poll;

const GATED_SCRIPT: &str = r#"
import { tool_registry_from } from "std/tools"

fn main(harness: Harness) {
  const tools = tool_registry_from([{
    name: "direct",
    description: "Wait at the registered host gate.",
    parameters: {value: {schema: {type: "string"}, required: true}},
    execution: {taskSupport: "optional"},
    handler: {args ->
      test_gate(args.value)
      test_mark("continued", args.value)
      return args.value
    },
  }])
  harness.tools.mcp_tools(tools)
  harness.tools.mcp_prompt({name: "gate", arguments: [{name: "value", required: true}], handler: {args ->
    test_gate(args.value)
    return args.value
  }})
}
"#;

struct Gates {
    config: GateConfigurator,
    events: mpsc::UnboundedReceiver<(String, String)>,
    tokens: mpsc::UnboundedReceiver<(String, Arc<AtomicBool>)>,
    _progress: mpsc::UnboundedReceiver<harn_vm::mcp_progress::ProgressContext>,
}

impl Gates {
    fn new() -> Self {
        let (events, event_rx) = mpsc::unbounded_channel();
        let (tokens, token_rx) = mpsc::unbounded_channel();
        let (progress, progress_rx) = mpsc::unbounded_channel();
        Self {
            config: GateConfigurator {
                events,
                progress,
                releases: Arc::new(
                    ["cancel", "other", "task", "old", "new", "queued", "prompt"]
                        .into_iter()
                        .map(|value| (value.into(), Arc::new(tokio::sync::Semaphore::new(0))))
                        .collect(),
                ),
                tokens: Some(tokens),
            },
            events: event_rx,
            tokens: token_rx,
            _progress: progress_rx,
        }
    }

    async fn registry(&self) -> (harn_vm::McpScriptRegistry, harn_vm::Vm) {
        load_registry_source(GATED_SCRIPT, |vm| {
            crate::VmConfigurator::configure(&self.config, vm).expect("gate configuration");
        })
        .await
    }

    fn release(&self, value: &str) {
        self.config.releases[value].add_permits(1);
    }

    async fn event(&mut self, kind: &str, value: &str) {
        assert_eq!(self.events.recv().await, Some((kind.into(), value.into())));
    }
}

fn params(value: &str) -> JsonValue {
    json!({"name": "direct", "arguments": {"value": value}})
}

struct Anchor {
    name: &'static str,
    dropped: mpsc::UnboundedSender<&'static str>,
}

impl Drop for Anchor {
    fn drop(&mut self) {
        self.dropped.send(self.name).expect("anchor observer");
    }
}

#[tokio::test]
async fn concurrent_http_script_calls_receive_distinct_tokens_and_cancel_independently() {
    local(async {
        let mut gates = Gates::new();
        let (registry, vm) = gates.registry().await;
        let runtime = McpScriptRuntime::spawn_local(registry, vm, ());
        let server = Arc::new(McpServer::new(McpServerConfig::script(runtime)));
        let cancelled = http_call(server.clone(), "cancel", "text/event-stream").await;
        let other = http_call(server, "other", "text/event-stream").await;
        let entered = BTreeSet::from([
            gates.events.recv().await.expect("first handler reached"),
            gates
                .events
                .recv()
                .await
                .expect("second handler reached before release"),
        ]);
        assert_eq!(
            entered,
            BTreeSet::from([
                ("entered".into(), "cancel".into()),
                ("entered".into(), "other".into()),
            ])
        );
        let first = gates.tokens.recv().await.expect("first actual VM token");
        let second = gates.tokens.recv().await.expect("second actual VM token");
        assert!(!Arc::ptr_eq(&first.1, &second.1));
        let tokens = HashMap::from([first, second]);
        drop(cancelled.into_body());
        assert!(tokens["cancel"].load(Ordering::SeqCst));
        assert!(!tokens["other"].load(Ordering::SeqCst));
        gates.release("cancel");
        gates.release("other");
        assert_eq!(
            BTreeSet::from([
                gates.events.recv().await.expect("first terminal witness"),
                gates.events.recv().await.expect("second terminal witness"),
            ]),
            BTreeSet::from([
                ("stopped".into(), "cancel".into()),
                ("continued".into(), "other".into()),
            ])
        );
        let body = axum::body::to_bytes(other.into_body(), 64 * 1024)
            .await
            .expect("other finishes");
        let body = String::from_utf8(body.to_vec()).expect("SSE UTF-8");
        assert!(body.contains("other"), "{body}");
        assert!(gates.events.try_recv().is_err());
    })
    .await;
}

#[tokio::test]
async fn closing_script_task_response_keeps_its_handler_and_result() {
    local(async {
        let mut gates = Gates::new();
        let (registry, vm) = gates.registry().await;
        let server = Arc::new(McpServer::new(McpServerConfig::script(
            McpScriptRuntime::spawn_local(registry, vm, ()),
        )));
        let (immediate, job) = match server
            .process_message(
                task_client_request(harn_vm::jsonrpc::request(1, "tools/call", params("task"))),
                SharedSession::new(),
                AuthRequest::default(),
            )
            .await
        {
            ImmediateResult::TaskStream { immediate, job } => (immediate, job),
            _ => panic!("script task admitted"),
        };
        let task_id = immediate["result"]["taskId"]
            .as_str()
            .expect("task id")
            .to_owned();
        let access = harn_vm::mcp_tasks::McpTaskAccess::unscoped();
        let notifier = server
            .tasks
            .notifier(&access, &task_id)
            .expect("known task");
        let completed = notifier.notified();
        tokio::pin!(completed);
        completed.as_mut().enable();
        let response =
            spawn_http_stream_with_prelude(server.clone(), *job, immediate).into_response();
        gates.event("entered", "task").await;
        let (name, token) = gates.tokens.recv().await.expect("task VM token");
        assert_eq!(name, "task");
        drop(response.into_body());
        assert!(!token.load(Ordering::SeqCst));
        gates.release("task");
        gates.event("continued", "task").await;
        completed.await;
        let result = server
            .tasks
            .handle_get(&access, json!(2), &json!({"taskId": task_id}));
        assert_eq!(result["result"]["status"], "completed");
        assert_eq!(result["result"]["result"]["content"][0]["text"], "task");
    })
    .await;
}

#[tokio::test]
async fn reload_preserves_inflight_anchor_and_admits_new_registry_without_waiting() {
    local(async {
        let mut gates = Gates::new();
        let (dropped, mut drops) = mpsc::unbounded_channel();
        let (registry, vm) = gates.registry().await;
        let (runtime, reloader) = McpScriptRuntime::spawn_local_reloadable(
            registry,
            vm,
            Anchor {
                name: "old",
                dropped: dropped.clone(),
            },
        );
        let old_runtime = runtime.clone();
        let old = tokio::task::spawn_local(async move {
            old_runtime
                .call_tool(
                    json!(1),
                    params("old"),
                    None,
                    false,
                    Arc::new(AtomicBool::new(false)),
                )
                .await
        });
        gates.event("entered", "old").await;
        let (registry, vm) = gates.registry().await;
        let mut changes = runtime.subscribe_changes();
        assert!(reloader.replace(
            registry,
            vm,
            Anchor {
                name: "new",
                dropped
            }
        ));
        changes.recv().await.expect("replacement applied");
        let new_runtime = runtime.clone();
        let new = tokio::task::spawn_local(async move {
            new_runtime
                .call_tool(
                    json!(2),
                    params("new"),
                    None,
                    false,
                    Arc::new(AtomicBool::new(false)),
                )
                .await
        });
        gates.event("entered", "new").await;
        gates.release("new");
        gates.event("continued", "new").await;
        let response = new.await.expect("new call completes before old release");
        assert_eq!(response["result"]["content"][0]["text"], "new");
        assert!(drops.try_recv().is_err(), "old VM anchor is still required");
        gates.release("old");
        gates.event("continued", "old").await;
        assert_eq!(
            old.await.expect("old call finishes")["result"]["content"][0]["text"],
            "old"
        );
        assert_eq!(drops.recv().await, Some("old"));
        drop(reloader);
        drop(runtime);
        assert_eq!(drops.recv().await, Some("new"));
    })
    .await;
}

#[tokio::test]
async fn abandoned_queued_receiver_skips_handler_before_entry_and_retires_anchor() {
    local(async {
        let mut gates = Gates::new();
        let (dropped, mut drops) = mpsc::unbounded_channel();
        let (registry, vm) = gates.registry().await;
        let runtime = McpScriptRuntime::spawn_local(
            registry,
            vm,
            Anchor {
                name: "retired",
                dropped,
            },
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let mut abandoned =
            Box::pin(runtime.call_tool(json!(1), params("queued"), None, false, cancel.clone()));
        assert!(matches!(futures::poll!(abandoned.as_mut()), Poll::Pending));
        drop(abandoned);
        assert!(cancel.load(Ordering::SeqCst));
        let positive_runtime = runtime.clone();
        let positive = tokio::task::spawn_local(async move {
            positive_runtime
                .call_tool(
                    json!(2),
                    params("other"),
                    None,
                    false,
                    Arc::new(AtomicBool::new(false)),
                )
                .await
        });
        gates.event("entered", "other").await;
        gates.release("other");
        gates.event("continued", "other").await;
        assert_eq!(
            positive.await.expect("positive call")["result"]["content"][0]["text"],
            "other"
        );
        drop(runtime);
        assert_eq!(drops.recv().await, Some("retired"));
        assert!(
            gates.events.try_recv().is_err(),
            "abandoned handler entered"
        );
        let (value, _) = gates.tokens.recv().await.expect("positive actual token");
        assert_eq!(value, "other");
        assert!(gates.tokens.try_recv().is_err());
    })
    .await;
}

#[tokio::test]
async fn successful_prompt_disarms_abandonment_without_cancelling_its_vm_token() {
    local(async {
        let mut gates = Gates::new();
        let (registry, vm) = gates.registry().await;
        let runtime = McpScriptRuntime::spawn_local(registry, vm, ());
        let prompt = tokio::task::spawn_local(async move {
            runtime
                .get_prompt(
                    json!(1),
                    json!({"name": "gate", "arguments": {"value": "prompt"}}),
                )
                .await
        });
        gates.event("entered", "prompt").await;
        let (value, token) = gates.tokens.recv().await.expect("prompt VM token");
        assert_eq!(value, "prompt");
        gates.release("prompt");
        let result = prompt.await.expect("prompt completed");
        assert_eq!(result["result"]["messages"][0]["content"]["text"], "prompt");
        assert!(
            !token.load(Ordering::SeqCst),
            "normal completion cancelled its VM"
        );
    })
    .await;
}

#[tokio::test]
async fn sequential_child_calls_preserve_captured_local_and_imported_module_state() {
    local(async {
        let directory = tempfile::tempdir().expect("module fixture");
        std::fs::write(
            directory.path().join("counter.harn"),
            r"
let count = 0
pub fn bump() -> int {
  count = count + 1
  return count
}
",
        )
        .expect("module counter");
        let source = r#"
import { tool_registry_from } from "std/tools"
import { bump } from "./counter.harn"
fn main(harness: Harness) {
  let local = 0
  const tools = tool_registry_from([{
    name: "next",
    description: "Increment the captured and module counters.",
    parameters: {},
    output_schema: {type: "object", properties: {local: {type: "integer"}, module: {type: "integer"}}, required: ["local", "module"], additionalProperties: false},
    handler: {_args ->
      local = local + 1
      return {local: local, module: bump()}
    },
  }])
  harness.tools.mcp_tools(tools)
}
"#;
        let (registry, vm) = load_registry_source(source, |vm| {
            vm.set_source_dir(directory.path());
            vm.set_source_info(
                &directory.path().join("main.harn").display().to_string(),
                source,
            );
        })
        .await;
        let runtime = McpScriptRuntime::spawn_local(registry, vm, ());
        for expected in 1..=3 {
            let result = runtime
                .call_tool(
                    json!(expected),
                    json!({"name": "next", "arguments": {}}),
                    None,
                    false,
                    Arc::new(AtomicBool::new(false)),
                )
                .await;
            assert_eq!(
                result["result"]["structuredContent"],
                json!({"local": expected, "module": expected}),
                "{result}"
            );
        }
    })
    .await;
}
