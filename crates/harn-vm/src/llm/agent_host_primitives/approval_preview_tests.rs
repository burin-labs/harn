//! A tool's `approval_preview` closure must reach the host permission request
//! on both ask paths (approval policy and side-effect ceiling), and must never
//! change the decision or the raw input. These tests compile real Harn
//! closures and drive the public dispatch primitive through a capturing ACP
//! bridge.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;

use super::host_agent_dispatch_tool_call;
use crate::bridge::HostBridge;
use crate::value::{DictMap, VmClosure, VmEnv, VmValue};

const HANDLER: &str = r#"fn handler(args: dict) -> string { return "verified" }"#;
const PREVIEW: &str = r#"fn preview(args: dict) -> dict {
  return {command: "python3 -m pytest", cwd: "/repo", summary: "Runs the project's tests"}
}"#;
const THROWING_PREVIEW: &str = r#"fn preview(args: dict) -> dict { throw "preview exploded" }"#;
const NIL_PREVIEW: &str = "fn preview(args: dict) { return nil }";

struct HostBridgeGuard(Option<Arc<HostBridge>>);

impl HostBridgeGuard {
    fn replace(bridge: Arc<HostBridge>) -> Self {
        Self(crate::llm::swap_current_host_bridge(Some(bridge)))
    }
}

impl Drop for HostBridgeGuard {
    fn drop(&mut self) {
        let _ = crate::llm::swap_current_host_bridge(self.0.take());
    }
}

fn compiled_closure(name: &str, source: &str) -> Arc<VmClosure> {
    let program = harn_parser::check_source_strict(source).expect("closure source parses");
    let chunk = crate::compiler::Compiler::new()
        .compile(&program)
        .expect("closure source compiles");
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name.as_str() == name)
        .expect("compiled function")
        .clone();
    Arc::new(VmClosure {
        func: function,
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    })
}

/// `{tools: [verify]}` with a no-argument handler, an execute annotation that
/// requires `process_exec`, and optionally an `approval_preview` closure.
fn verify_tools(preview_source: Option<&str>) -> VmValue {
    let mut entry = vec![
        ("name", VmValue::string("verify")),
        (
            "handler",
            VmValue::Closure(compiled_closure("handler", HANDLER)),
        ),
        (
            "annotations",
            crate::json_to_vm_value(&serde_json::json!({
                "kind": "execute",
                "side_effect_level": "process_exec",
            })),
        ),
    ];
    if let Some(source) = preview_source {
        entry.push((
            "approval_preview",
            VmValue::Closure(compiled_closure("preview", source)),
        ));
    }
    VmValue::dict([("tools", VmValue::List(Arc::new(vec![VmValue::dict(entry)])))])
}

fn capturing_bridge(
    outcome: serde_json::Value,
    requests: Arc<StdMutex<Vec<serde_json::Value>>>,
) -> Arc<HostBridge> {
    let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let response_pending = pending.clone();
    let writer = Arc::new(move |line: &str| {
        let request: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("invalid request: {error}"))?;
        requests
            .lock()
            .map_err(|_| "captured request mutex poisoned".to_string())?
            .push(request.clone());
        let id = request["id"]
            .as_u64()
            .ok_or_else(|| "bridge request missing numeric id".to_string())?;
        let sender = response_pending
            .try_lock()
            .map_err(|_| "bridge pending map unexpectedly locked".to_string())?
            .remove(&id)
            .ok_or_else(|| "bridge request was not pending".to_string())?;
        sender
            .send(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": outcome.clone()}))
            .map_err(|_| "bridge caller dropped before response".to_string())
    });
    Arc::new(HostBridge::from_parts_with_writer(
        pending,
        Arc::new(AtomicBool::new(false)),
        writer,
        1,
    ))
}

#[derive(Clone, Copy)]
enum AskPath {
    ApprovalPolicy,
    SideEffectCeiling,
}

fn options(session_id: &str, path: AskPath) -> DictMap {
    let mut options = DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        VmValue::string(session_id),
    );
    if matches!(path, AskPath::SideEffectCeiling) {
        options.insert(
            crate::value::intern_key("policy"),
            crate::json_to_vm_value(&serde_json::json!({
                "tools": ["verify"],
                "side_effect_level": "read_only",
            })),
        );
    }
    options
}

/// Dispatch a no-argument `verify` call that must be approved through `path`,
/// answering the permission request with `host_response`. Returns the tool
/// result and every bridge request the host received.
async fn dispatch_verify(
    path: AskPath,
    preview_source: Option<&str>,
    host_response: serde_json::Value,
) -> (serde_json::Value, Vec<serde_json::Value>) {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    if matches!(path, AskPath::ApprovalPolicy) {
        let policy: crate::orchestration::ToolApprovalPolicy =
            serde_json::from_value(serde_json::json!({
                "rules": [{"ask": {"tool": "verify"}, "reason": "verification runs commands"}]
            }))
            .expect("approval policy");
        crate::orchestration::push_approval_policy(policy);
    }
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let _bridge = HostBridgeGuard::replace(capturing_bridge(host_response, captured.clone()));
    let tools = verify_tools(preview_source);
    let call = crate::json_to_vm_value(&serde_json::json!({
        "id": "verify-call",
        "name": "verify",
        "arguments": {},
    }));
    let mut vm = crate::vm::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(vm),
        call,
        Some(&tools),
        &options("approval-preview-test", path),
    )
    .await
    .expect("dispatch returns a tool result");
    if matches!(path, AskPath::ApprovalPolicy) {
        crate::orchestration::pop_approval_policy();
    }
    crate::orchestration::clear_execution_policy_stacks();
    let requests = captured.lock().expect("captured requests").clone();
    (crate::llm::helpers::vm_value_to_json(&result), requests)
}

fn permission_tool_call(requests: &[serde_json::Value]) -> &serde_json::Value {
    let permission_requests: Vec<_> = requests
        .iter()
        .filter(|request| {
            request["method"] == crate::llm::acp_permission::METHOD_REQUEST_PERMISSION
        })
        .collect();
    assert_eq!(
        permission_requests.len(),
        1,
        "exactly one permission request: {requests:#?}"
    );
    &permission_requests[0]["params"]["toolCall"]
}

fn assert_preview_reached_host(tool_call: &serde_json::Value) {
    let expected = serde_json::json!({
        "command": "python3 -m pytest",
        "cwd": "/repo",
        "summary": "Runs the project's tests",
    });
    assert_eq!(tool_call["rawInput"], serde_json::json!({}));
    assert_eq!(tool_call["_meta"]["harn"]["approvalPreview"], expected);
    let content = tool_call["content"].as_array().expect("content blocks");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "content");
    assert_eq!(content[0]["content"]["type"], "text");
    assert!(content[0]["content"]["text"]
        .as_str()
        .expect("text")
        .contains("Command: python3 -m pytest"));
    assert_eq!(content[0]["_meta"]["harn"]["approval_preview"], expected);
    let evidence = tool_call["_meta"]["harn"]["approvalRequest"]["evidence_refs"]
        .as_array()
        .expect("evidence refs");
    assert!(evidence.iter().any(|evidence| {
        evidence["kind"] == "command_preview" && evidence["command"] == "python3 -m pytest"
    }));
}

fn assert_no_preview(tool_call: &serde_json::Value) {
    assert_eq!(tool_call["rawInput"], serde_json::json!({}));
    assert!(tool_call.get("content").is_none(), "{tool_call:#}");
    assert!(tool_call["_meta"]["harn"].get("approvalPreview").is_none());
    let evidence = tool_call["_meta"]["harn"]["approvalRequest"]["evidence_refs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(evidence
        .iter()
        .all(|evidence| evidence["kind"] != "command_preview"));
}

#[tokio::test(flavor = "current_thread")]
async fn approval_policy_ask_carries_the_declared_command() {
    let (result, requests) = dispatch_verify(
        AskPath::ApprovalPolicy,
        Some(PREVIEW),
        crate::llm::acp_permission::allow_response(),
    )
    .await;
    assert_preview_reached_host(permission_tool_call(&requests));
    assert_eq!(result["ok"], true, "{result:#}");
}

#[tokio::test(flavor = "current_thread")]
async fn side_effect_ceiling_ask_carries_the_declared_command() {
    let (result, requests) = dispatch_verify(
        AskPath::SideEffectCeiling,
        Some(PREVIEW),
        crate::llm::acp_permission::allow_response(),
    )
    .await;
    let tool_call = permission_tool_call(&requests);
    assert_eq!(
        tool_call["_meta"]["harn"]["policyDecision"]["source"],
        "side_effect_ceiling"
    );
    assert_preview_reached_host(tool_call);
    assert_eq!(result["ok"], true, "{result:#}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_preview_shows_nothing_and_keeps_the_decision() {
    for path in [AskPath::ApprovalPolicy, AskPath::SideEffectCeiling] {
        // A throwing preview: no preview, and the host's allow still runs it.
        let (result, requests) = dispatch_verify(
            path,
            Some(THROWING_PREVIEW),
            crate::llm::acp_permission::allow_response(),
        )
        .await;
        assert_no_preview(permission_tool_call(&requests));
        assert_eq!(result["ok"], true, "{result:#}");

        // A nil preview: no preview, and the host's rejection still refuses.
        let (result, requests) = dispatch_verify(
            path,
            Some(NIL_PREVIEW),
            crate::llm::acp_permission::reject_response(Some("no".to_string())),
        )
        .await;
        assert_no_preview(permission_tool_call(&requests));
        assert_eq!(result["ok"], false, "{result:#}");
        assert_eq!(result["denial"]["gate"], "host_rejected", "{result:#}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_tool_without_a_preview_is_unchanged() {
    for path in [AskPath::ApprovalPolicy, AskPath::SideEffectCeiling] {
        let (result, requests) =
            dispatch_verify(path, None, crate::llm::acp_permission::allow_response()).await;
        assert_no_preview(permission_tool_call(&requests));
        assert_eq!(result["ok"], true, "{result:#}");
    }
}
