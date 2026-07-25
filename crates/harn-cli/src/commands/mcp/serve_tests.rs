// Tests here mutate harn_vm process-global state (`HARN_STATE_DIR` env,
// thread-local `ACTIVE_EVENT_LOG`, trigger registry) through the shared
// `lock_harn_state_async` guard in `crate::tests::common::harn_state_lock`.
use super::*;
use axum::body::{to_bytes, Body};
use axum::extract::Form;
use axum::http::Request;
use axum::routing::post;
use axum::Router as AxumRouter;
use std::fs;
use std::path::Path;

use tempfile::TempDir;
use tower::ServiceExt;

use crate::env_guard::ScopedEnvVar;
use crate::tests::common::harn_state_lock::lock_harn_state_async;

#[path = "serve_tests/pagination.rs"]
mod pagination;

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn trigger_replay_steering_request_validates_pairs() {
    let request = TriggerReplayRequest {
        event_id: "evt-1".to_string(),
        as_of: None,
        steer_from: None,
        to_decision: Some(json!({"status": "skipped"})),
        reason: None,
        applied_by: None,
        scope: None,
    };
    assert!(trigger_replay_steering_from_request(&request).is_err());

    let request = TriggerReplayRequest {
        steer_from: Some("outcome".to_string()),
        scope: Some("this_persona".to_string()),
        ..request
    };
    let steering = trigger_replay_steering_from_request(&request)
        .expect("valid steering")
        .expect("steering present");
    assert_eq!(steering.step, "outcome");
    assert_eq!(steering.scope, harn_vm::CorrectionScope::ThisPersona);
}

fn fixture_args(temp: &TempDir) -> McpServeArgs {
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    McpServeArgs {
        local: OrchestratorLocalArgs {
            config: temp.path().join("harn.toml"),
            state_dir,
        },
        transport: McpServeTransport::Stdio,
        bind: "127.0.0.1:0".parse().unwrap(),
        path: "/mcp".to_string(),
        sse_path: "/sse".to_string(),
        messages_path: "/messages".to_string(),
    }
}

fn write_fixture(temp: &TempDir) {
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "cron-ok"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_ok"

[[triggers]]
id = "cron-fail"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_fail"
retry = { max = 1, backoff = "immediate", retention_days = 7 }
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_ok(event: TriggerEvent) -> dict {
  log("ok:" + event.kind)
  return {kind: event.kind, event_id: event.id, trace_id: event.trace_id}
}

pub fn on_fail(event: TriggerEvent) -> any {
  throw "boom:" + event.kind
}
"#,
    );
}

async fn init_session(service: &McpOrchestratorService) -> ConnectionState {
    let mut session = ConnectionState::default();
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "test-client", "version": "1.0.0" }
                }
            }),
        )
        .await;
    assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["resources"]["listChanged"],
        json!(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["resources"]["subscribe"],
        json!(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["prompts"]["listChanged"],
        json!(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["tasks"]["requests"]["tools"]["call"],
        json!({})
    );
    session
}

async fn call_tool(
    service: &McpOrchestratorService,
    session: &mut ConnectionState,
    name: &str,
    arguments: JsonValue,
) -> JsonValue {
    let response = service
        .handle_request(
            session,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments,
                }
            }),
        )
        .await;
    assert_eq!(response["result"]["isError"], false, "response={response}");
    response["result"]["structuredContent"].clone()
}

async fn read_resource(
    service: &McpOrchestratorService,
    session: &mut ConnectionState,
    uri: &str,
) -> JsonValue {
    let response = service
        .handle_request(
            session,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "resources/read",
                "params": { "uri": uri }
            }),
        )
        .await;
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    serde_json::from_str(text).unwrap_or_else(|_| json!(text))
}

// Wait for the next non-lagged notification, treating broadcast
// `Lagged` errors as benign (production listeners at lines 1154 /
// 1786 do the same). Without this, fixture-write storms during test
// setup can fill the 64-slot broadcast buffer faster than the
// subscriber drains it on a loaded CI runner, producing a spurious
// `Lagged(N)` panic.
async fn recv_next_notification(notifications: &mut broadcast::Receiver<JsonValue>) -> JsonValue {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for notification");
        match tokio::time::timeout(remaining, notifications.recv())
            .await
            .expect("timed out waiting for notification")
        {
            Ok(msg) => return msg,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => {
                panic!("notification channel closed")
            }
        }
    }
}

async fn recv_next_resource_notification(
    notifications: &mut broadcast::Receiver<McpResourceNotification>,
) -> McpResourceNotification {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for resource notification"
        );
        match tokio::time::timeout(remaining, notifications.recv())
            .await
            .expect("timed out waiting for resource notification")
        {
            Ok(msg) => return msg,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => {
                panic!("resource notification channel closed")
            }
        }
    }
}

async fn collect_notification_methods(
    notifications: &mut broadcast::Receiver<JsonValue>,
    expected: &[&str],
) -> std::collections::BTreeSet<String> {
    let expected = expected
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut seen = std::collections::BTreeSet::new();
    while !expected.iter().all(|method| seen.contains(*method)) {
        let notification = recv_next_notification(notifications).await;
        if let Some(method) = notification.get("method").and_then(JsonValue::as_str) {
            seen.insert(method.to_string());
        }
    }
    seen
}

async fn recv_next_task_notification(
    notifications: &mut broadcast::Receiver<McpTaskNotification>,
) -> McpTaskNotification {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for task notification"
        );
        match tokio::time::timeout(remaining, notifications.recv())
            .await
            .expect("timed out waiting for task notification")
        {
            Ok(msg) => return msg,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => {
                panic!("task notification channel closed")
            }
        }
    }
}

async fn recv_next_log_notification(
    notifications: &mut broadcast::Receiver<McpLogNotification>,
) -> McpLogNotification {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for log notification"
        );
        match tokio::time::timeout(remaining, notifications.recv())
            .await
            .expect("timed out waiting for log notification")
        {
            Ok(msg) => return msg,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => {
                panic!("log notification channel closed")
            }
        }
    }
}

#[test]
fn severity_for_event_honors_explicit_header_then_kind_then_default() {
    let binding = &LOG_STREAM_BINDINGS[0];
    let mut event = LogEvent::new("scan_completed", json!({}));
    assert_eq!(severity_for_event(binding, &event), binding.default_level);

    event.kind = "scan_failed".to_string();
    assert_eq!(
        severity_for_event(binding, &event),
        mcp_protocol::McpLogLevel::Warning
    );

    event.kind = "scan_error".to_string();
    assert_eq!(
        severity_for_event(binding, &event),
        mcp_protocol::McpLogLevel::Error
    );

    event.kind = "scan_completed".to_string();
    event
        .headers
        .insert("severity".to_string(), "alert".to_string());
    assert_eq!(
        severity_for_event(binding, &event),
        mcp_protocol::McpLogLevel::Alert
    );
}

#[tokio::test(flavor = "current_thread")]
async fn logging_set_level_updates_session_and_validates_input() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    assert_eq!(session.log_level, mcp_protocol::McpLogLevel::Info);

    let response = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                50,
                mcp_protocol::METHOD_LOGGING_SET_LEVEL,
                json!({"level": "warning"}),
            ),
        )
        .await;
    assert_eq!(response["result"], json!({}));
    assert_eq!(session.log_level, mcp_protocol::McpLogLevel::Warning);

    let bad_level = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                51,
                mcp_protocol::METHOD_LOGGING_SET_LEVEL,
                json!({"level": "loud"}),
            ),
        )
        .await;
    assert_eq!(bad_level["error"]["code"], json!(-32602));
    assert_eq!(session.log_level, mcp_protocol::McpLogLevel::Warning);

    let missing = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(52, mcp_protocol::METHOD_LOGGING_SET_LEVEL, json!({})),
        )
        .await;
    assert_eq!(missing["error"]["code"], json!(-32602));
}

#[tokio::test(flavor = "current_thread")]
async fn audit_events_emit_log_notifications_with_logger_and_level() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let _session = init_session(&service).await;
    let mut notifications = service.subscribe_log_notifications();
    service.wait_for_log_watchers_ready().await;

    let event_log = service
        .log_event_log
        .as_ref()
        .expect("log event log present in tests")
        .clone();
    let topic = Topic::new(harn_vm::SIGNATURE_VERIFY_AUDIT_TOPIC).unwrap();
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("severity".to_string(), "warning".to_string());
    event_log
        .append(
            &topic,
            LogEvent::new(
                "verify_failed",
                json!({"provider": "github", "reason": "bad signature"}),
            )
            .with_headers(headers),
        )
        .await
        .unwrap();

    let notification = recv_next_log_notification(&mut notifications).await;
    assert_eq!(notification.level, mcp_protocol::McpLogLevel::Warning);
    let params = &notification.message["params"];
    assert_eq!(params["level"], json!("warning"));
    assert_eq!(params["logger"], json!("harn.audit.signature_verify"));
    assert_eq!(params["data"]["kind"], json!("verify_failed"));
    assert_eq!(params["data"]["payload"]["provider"], json!("github"));
}

#[tokio::test(flavor = "current_thread")]
async fn resource_template_and_empty_prompt_lists_roundtrip() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let templates = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(10, "resources/templates/list", json!({})),
        )
        .await;
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        json!("harn://topic/{name}")
    );
    assert_eq!(
        templates["result"]["resourceTemplates"][1]["uriTemplate"],
        json!("harn://event/{event_id}")
    );
    assert_eq!(
        templates["result"]["resourceTemplates"][2]["uriTemplate"],
        json!("harn://dlq/{entry_id}")
    );

    let topic_completion = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                9,
                mcp_protocol::METHOD_COMPLETION_COMPLETE,
                json!({
                    "ref": {"type": "ref/resource", "uri": "harn://topic/{name}"},
                    "argument": {"name": "name", "value": "trigger."}
                }),
            ),
        )
        .await;
    assert_eq!(
        topic_completion["result"]["completion"]["values"],
        json!(["trigger.inbox", "trigger.outbox"])
    );

    let prompts = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(11, "prompts/list", json!({})),
        )
        .await;
    assert_eq!(prompts["result"]["prompts"], json!([]));

    let prompt = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(12, "prompts/get", json!({"name": "missing"})),
        )
        .await;
    assert_eq!(prompt["error"]["code"], json!(-32602));
}

#[tokio::test(flavor = "current_thread")]
async fn file_backed_prompts_list_render_and_notify_changes() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    write_file(temp.path(), "pixel.png", "fake");
    write_file(
        temp.path(),
        "review.harn.prompt",
        r#"---
id = "review"
description = "Review code"
images = [{ path = "pixel.png", mime_type = "image/png" }]
[[arguments]]
name = "code"
description = "Code to review"
required = true
[[arguments]]
name = "language"
required = false
suggestions = ["rust", "ruby", "typescript"]
---
Review this {{ language }}: {{ code }}
"#,
    );
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    let mut notifications = service.subscribe_list_notifications();

    let prompts = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(20, "prompts/list", json!({})),
        )
        .await;
    assert_eq!(prompts["result"]["prompts"][0]["name"], json!("review"));
    assert_eq!(
        prompts["result"]["prompts"][0]["arguments"][0]["description"],
        json!("Code to review")
    );

    let completion = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                19,
                mcp_protocol::METHOD_COMPLETION_COMPLETE,
                json!({
                    "ref": {"type": "ref/prompt", "name": "review"},
                    "argument": {"name": "language", "value": "ru"},
                }),
            ),
        )
        .await;
    assert_eq!(
        completion["result"]["completion"]["values"],
        json!(["ruby", "rust"])
    );
    assert_eq!(completion["result"]["completion"]["total"], json!(2));
    assert_eq!(completion["result"]["completion"]["hasMore"], json!(false));

    let missing = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(21, "prompts/get", json!({"name": "review"})),
        )
        .await;
    assert_eq!(missing["error"]["code"], json!(-32602));

    let prompt = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                22,
                "prompts/get",
                json!({"name": "review", "arguments": {"code": "fn main() {}"}}),
            ),
        )
        .await;
    assert!(prompt["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("fn main"));
    assert_eq!(
        prompt["result"]["messages"][1]["content"]["type"],
        json!("image")
    );
    assert_eq!(
        prompt["result"]["messages"][1]["content"]["data"],
        json!("ZmFrZQ==")
    );

    write_file(
        temp.path(),
        "review.harn.prompt",
        r#"---
id = "review"
[[arguments]]
name = "code"
required = true
---
Updated: {{ code }}
"#,
    );
    service.notify_manifest_reloaded();
    // Exercise the shared reload/broadcast path in-process. The production
    // file watcher uses the same cache-refresh and list-change helpers, but
    // tests should not depend on OS notification timing under release-audit
    // load.
    let seen =
        collect_notification_methods(&mut notifications, &["notifications/prompts/list_changed"])
            .await;
    assert!(seen.contains("notifications/prompts/list_changed"));
}

#[tokio::test(flavor = "current_thread")]
async fn package_metadata_changes_notify_tools_resources_and_prompts() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut notifications = service.subscribe_list_notifications();

    write_file(
        temp.path(),
        "harn.lock",
        r#"
[[package]]
name = "prompt-pack"
version = "0.1.0"
"#,
    );
    service.notify_manifest_reloaded();

    let seen = collect_notification_methods(
        &mut notifications,
        &[
            "notifications/tools/list_changed",
            "notifications/resources/list_changed",
            "notifications/prompts/list_changed",
        ],
    )
    .await;
    assert!(seen.contains("notifications/tools/list_changed"));
    assert!(seen.contains("notifications/resources/list_changed"));
    assert!(seen.contains("notifications/prompts/list_changed"));
}

#[tokio::test(flavor = "current_thread")]
async fn tools_list_advertises_tool_metadata_per_tool() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let response = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(30, "tools/list", json!({})),
        )
        .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let trigger_fire = tools
        .iter()
        .find(|tool| tool["name"] == "harn.trigger.fire")
        .unwrap();
    let trigger_list = tools
        .iter()
        .find(|tool| tool["name"] == "harn.trigger.list")
        .unwrap();
    assert_eq!(trigger_fire["execution"]["taskSupport"], json!("optional"));
    assert_eq!(trigger_fire["annotations"]["readOnlyHint"], json!(false));
    assert_eq!(trigger_fire["annotations"]["destructiveHint"], json!(true));
    assert_eq!(trigger_fire["annotations"]["openWorldHint"], json!(true));
    assert_eq!(trigger_list["execution"]["taskSupport"], json!("forbidden"));
    assert_eq!(trigger_list["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(trigger_list["annotations"]["idempotentHint"], json!(true));
    assert_eq!(trigger_list["annotations"]["openWorldHint"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_rejects_task_augmentation() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let response = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                100,
                "tools/call",
                json!({
                    "name": "harn.trigger.list",
                    "arguments": {},
                    "task": {"title": "async please"}
                }),
            ),
        )
        .await;
    assert_eq!(response["error"]["code"], json!(-32602));
    assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_fire_task_roundtrip_polls_and_retrieves_result() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    let mut task_notifications = service.subscribe_task_notifications();

    let created = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                101,
                "tools/call",
                json!({
                    "name": "harn.trigger.fire",
                    "arguments": {
                        "trigger_id": "cron-ok",
                        "payload": {}
                    },
                    "task": {"ttl": 60_000}
                }),
            ),
        )
        .await;
    assert_eq!(created["result"]["task"]["status"], json!("working"));
    assert_eq!(created["result"]["task"]["ttl"], json!(60_000));
    let task_id = created["result"]["task"]["taskId"].as_str().unwrap();

    let listed = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(102, "tasks/list", json!({})),
        )
        .await;
    assert_eq!(listed["result"]["tasks"][0]["taskId"], json!(task_id));

    let result = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(103, "tasks/result", json!({ "taskId": task_id })),
        )
        .await;
    assert_eq!(result["result"]["isError"], json!(false), "result={result}");
    assert_eq!(
        result["result"]["_meta"][mcp_protocol::RELATED_TASK_META_KEY]["taskId"],
        json!(task_id)
    );
    assert_eq!(
        result["result"]["structuredContent"]["status"],
        json!("dispatched")
    );

    let task = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(104, "tasks/get", json!({ "taskId": task_id })),
        )
        .await;
    assert_eq!(task["result"]["status"], json!("completed"));

    let mut statuses = std::collections::BTreeSet::new();
    while !statuses.contains("completed") {
        let notification = recv_next_task_notification(&mut task_notifications).await;
        if notification.owner == session.client_identity {
            let status = notification.message["params"]["status"].as_str().unwrap();
            statuses.insert(status.to_string());
        }
    }
    assert!(statuses.contains("working"));
    assert!(statuses.contains("completed"));
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_list_tool_returns_manifest_bindings() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let result = call_tool(&service, &mut session, "harn.trigger.list", json!({})).await;
    let triggers = result["triggers"].as_array().unwrap();
    assert_eq!(triggers.len(), 2);
    assert!(triggers
        .iter()
        .any(|trigger| trigger["trigger_id"] == "cron-ok"));
    assert!(triggers
        .iter()
        .any(|trigger| trigger["trigger_id"] == "cron-fail"));
}

#[tokio::test(flavor = "current_thread")]
async fn secret_scan_tool_returns_findings_and_audits_them() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let result = call_tool(
        &service,
        &mut session,
        "harn.secret_scan",
        json!({
            "content": r#"token = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB""#,
        }),
    )
    .await;
    let findings = result.as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["detector"], "github-token");

    let ctx = load_local_runtime(&service.local_args()).await.unwrap();
    let events = read_topic(&ctx.event_log, harn_vm::SECRET_SCAN_AUDIT_TOPIC)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.payload["caller"], "mcp.harn.secret_scan");
    assert_eq!(events[0].1.payload["finding_count"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_fire_roundtrip_records_event_resource_and_action_graph() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let fire = call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({
            "trigger_id": "cron-ok",
            "payload": {
                "headers": { "x-test": "1" }
            }
        }),
    )
    .await;
    assert_eq!(fire["status"], "dispatched");
    let event_id = fire["event_id"].as_str().unwrap();
    let event = read_resource(&service, &mut session, &format!("harn://event/{event_id}")).await;
    assert_eq!(
        event["event"]["headers"]["x-harn-mcp-client"],
        "test-client/1.0.0"
    );

    let ctx = load_local_runtime(&service.local_args()).await.unwrap();
    let action_graph = read_topic(&ctx.event_log, ACTION_GRAPH_TOPIC)
        .await
        .unwrap();
    assert!(
        action_graph.iter().any(|(_, event)| {
            event.payload["context"]["tool_name"] == json!("harn.trigger.fire")
        }),
        "action_graph={action_graph:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn resource_subscription_notifies_when_event_log_topic_changes() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    let mut notifications = service.subscribe_resource_notifications();

    let subscribed = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                10,
                "resources/subscribe",
                json!({ "uri": "harn://topic/trigger.outbox" }),
            ),
        )
        .await;
    assert_eq!(subscribed["result"], json!({}));
    assert!(session
        .subscribed_resources
        .contains("harn://topic/trigger.outbox"));

    let fire = call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({ "trigger_id": "cron-ok", "payload": {} }),
    )
    .await;
    assert_eq!(fire["status"], "dispatched");

    let notification = recv_next_resource_notification(&mut notifications).await;
    assert_eq!(notification.uri, "harn://topic/trigger.outbox");
    assert_eq!(
        notification.message["method"],
        json!("notifications/resources/updated")
    );
    assert_eq!(
        notification.message["params"]["uri"],
        json!("harn://topic/trigger.outbox")
    );

    let topic = read_resource(&service, &mut session, "harn://topic/trigger.outbox").await;
    assert_eq!(topic["topic"], json!("trigger.outbox"));
    assert!(topic["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));

    let unsubscribed = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(
                11,
                "resources/unsubscribe",
                json!({ "uri": "harn://topic/trigger.outbox" }),
            ),
        )
        .await;
    assert_eq!(unsubscribed["result"], json!({}));
    assert!(!session
        .subscribed_resources
        .contains("harn://topic/trigger.outbox"));
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_replay_tool_replays_event() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    let fire = call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({ "trigger_id": "cron-ok", "payload": {} }),
    )
    .await;
    let replay = call_tool(
        &service,
        &mut session,
        "harn.trigger.replay",
        json!({ "event_id": fire["event_id"] }),
    )
    .await;
    assert_eq!(replay["status"], "dispatched");
    assert_eq!(replay["replay_of_event_id"], fire["event_id"]);
}

#[tokio::test(flavor = "current_thread")]
async fn dlq_tools_roundtrip_and_resource_read() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let fire = call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({ "trigger_id": "cron-fail", "payload": {} }),
    )
    .await;
    assert_eq!(fire["status"], "dlq");
    let entries = call_tool(
        &service,
        &mut session,
        "harn.orchestrator.dlq.list",
        json!({}),
    )
    .await;
    let entry_id = entries["entries"][0]["id"].as_str().unwrap();
    let detail = read_resource(&service, &mut session, &format!("harn://dlq/{entry_id}")).await;
    assert_eq!(detail["id"], entry_id);

    let retry = call_tool(
        &service,
        &mut session,
        "harn.orchestrator.dlq.retry",
        json!({ "entry_id": entry_id }),
    )
    .await;
    assert_eq!(retry["entry_id"], entry_id);
    assert_eq!(retry["handle"]["replay_of_event_id"], fire["event_id"]);
}

#[tokio::test(flavor = "current_thread")]
async fn queue_and_inspect_tools_return_snapshots() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let _ = call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({ "trigger_id": "cron-ok", "payload": {} }),
    )
    .await;
    let queue = call_tool(&service, &mut session, "harn.orchestrator.queue", json!({})).await;
    assert!(queue["outbox"]["count"].as_u64().unwrap() >= 1);

    let inspect = call_tool(
        &service,
        &mut session,
        "harn.orchestrator.inspect",
        json!({}),
    )
    .await;
    assert_eq!(inspect["triggers"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn eval_inspect_run_reports_artifacts_and_event_chain() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let bundle = temp.path().join("eval-run");
    write_file(
        &bundle,
        "summary.json",
        r#"{
          "task": "zig-feat",
          "model": "fw-gpt-oss-120b",
          "result": "FAIL",
          "outcome_kind": "edit_applied_but_wrong",
          "verification_passed": false,
          "verify_timed_out": false,
          "verify_failure_excerpt": "src/schema.zig:1:1: error: file exists in modules",
          "cross_check": {
            "e1_verdict": "fail",
            "e2_verdict": "fail",
            "e1_e2_aligned": true,
            "e2_reason": "re-exec verify FAILED in clean checkout (exit 1)",
            "judge_dead": true
          },
          "completion_contract": {
            "contract_passed": true,
            "run_ready_for_final": false,
            "verification_passed": false,
            "completion_warnings": ["missing_exact_done_sentinel"]
          },
          "verification": "FAIL",
          "run_record_path": null
        }"#,
    );
    write_file(
        &bundle,
        "artifacts/final.diff",
        "diff --git a/src/schema.zig b/src/schema.zig\n",
    );
    write_file(
        &bundle,
        "events/topics/agent.transcript.llm.jsonl",
        r#"{"id":1,"event":{"kind":"message","payload":{"role":"user"},"headers":{"harn.provenance.record_hash":"sha256:a"},"occurred_at_ms":1}}"#,
    );
    write_file(
        &bundle,
        "events/topics/observability.agent_events.session-1.jsonl",
        concat!(
            r#"{"id":1,"event":{"kind":"tool","payload":{"event":{"type":"tool_call","tool_call_id":"tc1"}},"headers":{"harn.provenance.record_hash":"sha256:a"},"occurred_at_ms":1}}"#,
            "\n",
            r#"{"id":2,"event":{"kind":"tool","payload":{"event":{"type":"tool_call_update","tool_call_id":"tc1"}},"headers":{"harn.provenance.prev_hash":"sha256:a","harn.provenance.record_hash":"sha256:b"},"occurred_at_ms":2}}"#,
            "\n",
        ),
    );
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let dossier = call_tool(
        &service,
        &mut session,
        "harn.eval.inspect_run",
        json!({ "output_dir": bundle, "limit": 1 }),
    )
    .await;

    assert_eq!(dossier["verdict"]["result"], "FAIL");
    assert_eq!(dossier["event_chain"]["has_agent_transcript"], true);
    assert_eq!(dossier["event_chain"]["tool_call_events"], 1);
    assert_eq!(dossier["event_chain"]["tool_call_update_events"], 1);
    let agent_events = dossier["event_topics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|topic| topic["topic"] == "observability.agent_events.session-1")
        .unwrap();
    assert_eq!(agent_events["record_count"], 2);
    assert_eq!(agent_events["sampled_event_count"], 1);
    assert_eq!(agent_events["counts_complete"], true);
    // Full-scan ids span both records; the sample-scoped ids cover only the
    // first record (limit=1), so `last_sampled_id` must not leak the tail.
    assert_eq!(agent_events["first_event_id"], 1);
    assert_eq!(agent_events["last_event_id"], 2);
    assert_eq!(agent_events["first_sampled_id"], 1);
    assert_eq!(agent_events["last_sampled_id"], 1);
    assert_eq!(agent_events["provenance"]["chain_breaks_in_sample"], 0);
    assert_eq!(agent_events["provenance"]["sample_chain_ok"], true);
    assert!(dossier["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|gap| gap == "missing run_record or run_record_path is null"));
}

#[tokio::test(flavor = "current_thread")]
async fn trust_query_returns_filtered_trace_groups() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let ctx = load_local_runtime(&service.local_args()).await.unwrap();
    harn_vm::append_trust_record(
        &ctx.event_log,
        &harn_vm::TrustRecord::new(
            "ide-bot",
            "issue.opened",
            None,
            harn_vm::TrustOutcome::Success,
            "trace-1",
            harn_vm::AutonomyTier::ActAuto,
        ),
    )
    .await
    .unwrap();
    harn_vm::append_trust_record(
        &ctx.event_log,
        &harn_vm::TrustRecord::new(
            "ide-bot",
            "issue.closed",
            None,
            harn_vm::TrustOutcome::Success,
            "trace-2",
            harn_vm::AutonomyTier::ActAuto,
        ),
    )
    .await
    .unwrap();
    harn_vm::append_trust_record(
        &ctx.event_log,
        &harn_vm::TrustRecord::new(
            "ide-bot",
            "issue.commented",
            None,
            harn_vm::TrustOutcome::Failure,
            "trace-2",
            harn_vm::AutonomyTier::ActAuto,
        ),
    )
    .await
    .unwrap();

    let result = call_tool(
        &service,
        &mut session,
        "harn.trust.query",
        json!({
            "agent": "ide-bot",
            "grouped_by_trace": true,
            "limit": 2
        }),
    )
    .await;
    assert_eq!(result["grouped_by_trace"], json!(true));
    assert_eq!(result["results"].as_array().unwrap().len(), 1);
    assert_eq!(result["results"][0]["trace_id"], "trace-2");
    assert_eq!(result["results"][0]["records"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn manifest_resource_reads_raw_manifest() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;

    let manifest = read_resource(&service, &mut session, "harn://manifest").await;
    let manifest = manifest.as_str().unwrap();
    assert!(manifest.contains("[[triggers]]"));
    assert!(manifest.contains("cron-ok"));
}

#[tokio::test(flavor = "current_thread")]
async fn streamable_http_endpoint_supports_sse_get_delete_and_session_headers() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(
        service.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    );

    let init = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json, text/event-stream")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": MCP_PROTOCOL_VERSION,
                            "capabilities": {},
                            "clientInfo": { "name": "streamable-test", "version": "1.0.0" }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::OK);
    assert_eq!(
        init.headers()
            .get(MCP_PROTOCOL_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(MCP_PROTOCOL_VERSION)
    );
    let session_id = init
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .expect("session id")
        .to_string();

    let tools = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "text/event-stream")
                .header(MCP_SESSION_HEADER, &session_id)
                .header("content-type", "application/json")
                .body(Body::from(
                    harn_vm::jsonrpc::request(2, "tools/list", json!({})).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tools.status(), StatusCode::OK);
    assert!(tools
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream")));
    let body = to_bytes(tools.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("event: message"), "{body}");
    assert!(body.contains("harn.trigger.list"), "{body}");

    let get = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header("accept", "text/event-stream")
                .header(MCP_SESSION_HEADER, &session_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    assert!(get
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream")));
    let mut stream = get.into_body().into_data_stream();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("timed out waiting for SSE prime event")
        .expect("SSE stream ended")
        .expect("SSE body error");
    service.notify_manifest_reloaded();
    let mut streamed = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !streamed.contains("notifications/tools/list_changed")
        || !streamed.contains("notifications/resources/list_changed")
    {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for list_changed SSE notifications; body={streamed}"
        );
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for SSE notification")
            .expect("SSE stream ended")
            .expect("SSE body error");
        streamed.push_str(std::str::from_utf8(&chunk).unwrap());
    }

    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header(MCP_SESSION_HEADER, &session_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let after_delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header(MCP_SESSION_HEADER, &session_id)
                .header("content-type", "application/json")
                .body(Body::from(
                    harn_vm::jsonrpc::request(3, "tools/list", json!({})).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_metadata_and_challenge_are_served_when_configured() {
    // Acquire the harn-state lock BEFORE setting env vars. Rust drops
    // bindings in reverse declaration order, so env vars must be
    // declared *after* the lock to be cleared before another test
    // can acquire `lock_harn_state()` and read leaked OAuth config.
    let _guard = lock_harn_state_async().await;
    let _auth_servers = ScopedEnvVar::set(
        "HARN_MCP_OAUTH_AUTHORIZATION_SERVERS",
        "https://auth.example.test",
    );
    let _introspection = ScopedEnvVar::set(
        "HARN_MCP_OAUTH_INTROSPECTION_URL",
        "https://auth.example.test/introspect",
    );
    let _resource = ScopedEnvVar::set("HARN_MCP_OAUTH_RESOURCE", "https://mcp.example.test/mcp");
    let _audience = ScopedEnvVar::set("HARN_MCP_OAUTH_AUDIENCE", "https://mcp.example.test/mcp");
    let _scopes = ScopedEnvVar::set("HARN_MCP_OAUTH_SCOPES", "harn:mcp");
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let router = http_router_for_local(
        args.local.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    )
    .unwrap();

    let metadata = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/.well-known/oauth-protected-resource/mcp")
                .header("host", "mcp.example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    let body = to_bytes(metadata.into_body(), usize::MAX).await.unwrap();
    let metadata: JsonValue = serde_json::from_slice(&body).unwrap();
    assert_eq!(metadata["resource"], json!("https://mcp.example.test/mcp"));
    assert_eq!(
        metadata["authorization_servers"],
        json!(["https://auth.example.test"])
    );
    assert_eq!(metadata["scopes_supported"], json!(["harn:mcp"]));

    let unauthorized = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "mcp.example.test")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .body(Body::from(
                    harn_vm::jsonrpc::request(1, "initialize", json!({})).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let challenge = unauthorized
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(challenge.starts_with("Bearer "), "{challenge}");
    assert!(
        challenge.contains(
            "resource_metadata=\"http://mcp.example.test/.well-known/oauth-protected-resource/mcp\""
        ),
        "{challenge}"
    );
    assert!(challenge.contains("scope=\"harn:mcp\""), "{challenge}");
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_json_descriptor_advertises_streamable_http_endpoint() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let router = http_router_for_local(
        args.local.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    )
    .unwrap();

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/.well-known/mcp.json")
                .header("host", "internal.invalid")
                .header("x-forwarded-host", "mcp.example.test")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let descriptor: JsonValue = serde_json::from_slice(&body).unwrap();
    assert_eq!(descriptor["name"], json!("Harn MCP"));
    assert_eq!(
        descriptor["endpoint"],
        json!("https://mcp.example.test/mcp")
    );
    assert!(descriptor["description"].as_str().unwrap().contains("Harn"));
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_introspection_accepts_valid_token_and_rejects_wrong_audience() {
    async fn introspect(Form(form): Form<BTreeMap<String, String>>) -> Json<JsonValue> {
        match form.get("token").map(String::as_str) {
            Some("valid-token") => Json(json!({
                "active": true,
                "aud": "mcp://harn-test",
                "scope": "harn:mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600
            })),
            Some("wrong-audience") => Json(json!({
                "active": true,
                "aud": "mcp://other",
                "scope": "harn:mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600
            })),
            Some("expired-token") => Json(json!({
                "active": true,
                "aud": "mcp://harn-test",
                "scope": "harn:mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() - 1
            })),
            Some("missing-scope") => Json(json!({
                "active": true,
                "aud": "mcp://harn-test",
                "scope": "other:scope",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600
            })),
            _ => Json(json!({ "active": false })),
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth_addr = listener.local_addr().unwrap();
    let auth_server = tokio::spawn(async move {
        axum::serve(
            listener,
            AxumRouter::new().route("/introspect", post(introspect)),
        )
        .await
        .unwrap();
    });

    // Acquire the harn-state lock BEFORE setting env vars so they
    // are dropped (cleared) before another test can re-enter the
    // lock — see the matching comment in
    // `oauth_metadata_and_challenge_are_served_when_configured`.
    let _guard = lock_harn_state_async().await;
    let auth_server_url = format!("http://{auth_addr}");
    let introspection_url = format!("{auth_server_url}/introspect");
    let _auth_servers = ScopedEnvVar::set("HARN_MCP_OAUTH_AUTHORIZATION_SERVERS", &auth_server_url);
    let _introspection = ScopedEnvVar::set("HARN_MCP_OAUTH_INTROSPECTION_URL", &introspection_url);
    let _audience = ScopedEnvVar::set("HARN_MCP_OAUTH_AUDIENCE", "mcp://harn-test");
    let _scopes = ScopedEnvVar::set("HARN_MCP_OAUTH_SCOPES", "harn:mcp");
    let _resource = ScopedEnvVar::set("HARN_MCP_OAUTH_RESOURCE", "mcp://harn-test");
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let router = http_router_for_local(
        args.local.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    )
    .unwrap();

    let initialize_body = Body::from(
        harn_vm::jsonrpc::request(
            1,
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "oauth-test", "version": "1.0.0" }
            }),
        )
        .to_string(),
    );
    let valid = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer valid-token")
                .body(initialize_body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(valid.status(), StatusCode::OK);

    for token in ["wrong-audience", "expired-token"] {
        let rejected = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        harn_vm::jsonrpc::request(1, "initialize", json!({})).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED, "token={token}");
        assert!(rejected
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|challenge| challenge.contains("error=\"invalid_token\"")));
    }

    let insufficient_scope = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer missing-scope")
                .body(Body::from(
                    harn_vm::jsonrpc::request(1, "initialize", json!({})).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(insufficient_scope.status(), StatusCode::FORBIDDEN);
    assert!(insufficient_scope
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(
            |challenge| challenge.contains("error=\"insufficient_scope\"")
                && challenge.contains("scope=\"harn:mcp\"")
        ));

    auth_server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_sse_routes_are_marked_deprecated() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let router = http_router_for_local(
        args.local.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    )
    .unwrap();

    let sse = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/sse")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sse.status(), StatusCode::OK);
    assert_eq!(
        sse.headers()
            .get(DEPRECATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    drop(sse);

    let messages = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/messages")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(messages.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        messages
            .headers()
            .get(DEPRECATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
}

fn rc_meta() -> JsonValue {
    json!({
        mcp_protocol::RC_META_KEY_PROTOCOL_VERSION: mcp_protocol::DRAFT_PROTOCOL_VERSION,
        mcp_protocol::RC_META_KEY_CLIENT_INFO: {"name": "rc-client", "version": "1.0"},
        mcp_protocol::RC_META_KEY_CLIENT_CAPABILITIES: {},
    })
}

#[tokio::test(flavor = "current_thread")]
async fn server_discover_returns_capabilities_and_skips_initialize() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = McpOrchestratorService::new_local(args.local.clone()).unwrap();
    let mut session = ConnectionState::default();
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": mcp_protocol::METHOD_SERVER_DISCOVER,
                "params": {}
            }),
        )
        .await;
    assert_eq!(
        response["result"]["resultType"],
        json!(mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(
        response["result"]["protocolVersion"],
        json!(mcp_protocol::DRAFT_PROTOCOL_VERSION)
    );
    let supported = response["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions array");
    assert!(supported
        .iter()
        .any(|v| v == &json!(mcp_protocol::DRAFT_PROTOCOL_VERSION)));
    assert!(supported.iter().any(|v| v == &json!(MCP_PROTOCOL_VERSION)));
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    assert!(session.initialized);
    assert!(session.authenticated);
}

#[tokio::test(flavor = "current_thread")]
async fn rc_tools_list_returns_result_type_and_cache_hint_without_initialize() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = McpOrchestratorService::new_local(args.local.clone()).unwrap();
    // No prior initialize: a Modern client should still be able to call
    // tools/list with `_meta` metadata.
    let mut session = ConnectionState::default();
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": { "_meta": rc_meta() }
            }),
        )
        .await;
    assert_eq!(
        response["result"]["resultType"],
        json!(mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(
        response["result"]["ttlMs"],
        json!(mcp_protocol::DEFAULT_LIST_CACHE_TTL_MS)
    );
    assert_eq!(
        response["result"]["cacheScope"],
        json!(mcp_protocol::DEFAULT_LIST_CACHE_SCOPE)
    );
    assert!(response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .any(|tool| tool["name"] == json!("harn.trigger.list")));
    assert!(session.initialized);
    assert_eq!(session.protocol_mode, mcp_protocol::McpProtocolMode::Modern);
}

#[tokio::test(flavor = "current_thread")]
async fn rc_request_with_unsupported_protocol_version_returns_minus_32004() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = McpOrchestratorService::new_local(args.local.clone()).unwrap();
    let mut session = ConnectionState::default();
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        mcp_protocol::RC_META_KEY_PROTOCOL_VERSION: "2099-01-01"
                    }
                }
            }),
        )
        .await;
    assert_eq!(
        response["error"]["code"],
        json!(mcp_protocol::UNSUPPORTED_PROTOCOL_VERSION_CODE)
    );
    let supported = response["error"]["data"]["supported"]
        .as_array()
        .expect("supported array");
    assert!(supported
        .iter()
        .any(|v| v == &json!(mcp_protocol::DRAFT_PROTOCOL_VERSION)));
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_initialize_still_negotiates_stable_protocol_version() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = McpOrchestratorService::new_local(args.local.clone()).unwrap();
    let mut session = ConnectionState::default();
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "legacy", "version": "1.0" }
                }
            }),
        )
        .await;
    assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    // Legacy responses must remain free of RC envelope fields so the
    // existing 2025-11-25 wire shape stays byte-stable.
    assert!(response["result"].get("resultType").is_none());
    assert!(response["result"].get("ttlMs").is_none());
    assert_eq!(session.protocol_mode, mcp_protocol::McpProtocolMode::Legacy);
}

#[tokio::test(flavor = "current_thread")]
async fn http_rc_request_returns_no_session_id_and_negotiates_draft_version() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(
        service.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(MCP_PROTOCOL_HEADER, mcp_protocol::DRAFT_PROTOCOL_VERSION)
                .header(mcp_protocol::RC_HEADER_METHOD, "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/list",
                        "params": { "_meta": rc_meta() }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(MCP_PROTOCOL_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(mcp_protocol::DRAFT_PROTOCOL_VERSION),
        "RC responses must echo the negotiated draft version"
    );
    assert!(
        response.headers().get(MCP_SESSION_HEADER).is_none(),
        "RC responses must not emit MCP-Session-Id"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: JsonValue = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body["result"]["resultType"],
        json!(mcp_protocol::RESULT_TYPE_COMPLETE)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_rc_request_rejects_method_header_mismatch() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(
        service.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(MCP_PROTOCOL_HEADER, mcp_protocol::DRAFT_PROTOCOL_VERSION)
                .header(mcp_protocol::RC_HEADER_METHOD, "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "tools/call",
                        "params": { "name": "harn.trigger.list", "_meta": rc_meta() }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: JsonValue = serde_json::from_slice(&body).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], json!(-32600));
    assert_eq!(body["error"]["data"]["headerValue"], "tools/list");
    assert_eq!(body["error"]["data"]["bodyMethod"], "tools/call");
}

#[tokio::test(flavor = "current_thread")]
async fn rc_http_get_stream_works_without_session_id() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(
        service.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header("accept", "text/event-stream")
                .header(MCP_PROTOCOL_HEADER, mcp_protocol::DRAFT_PROTOCOL_VERSION)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(MCP_PROTOCOL_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(mcp_protocol::DRAFT_PROTOCOL_VERSION)
    );
    assert!(response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream")));

    let mut stream = response.into_body().into_data_stream();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("timed out waiting for SSE prime event")
        .expect("SSE stream ended")
        .expect("SSE body error");
    service.notify_manifest_reloaded();
    let mut buffer = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !buffer.contains("notifications/tools/list_changed") {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "RC GET stream did not receive list_changed notification; body={buffer}"
        );
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for SSE notification")
            .expect("SSE stream ended")
            .expect("SSE body error");
        buffer.push_str(std::str::from_utf8(&chunk).unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_initialize_http_path_still_returns_session_id() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(
        service.clone(),
        "/mcp".to_string(),
        "/sse".to_string(),
        "/messages".to_string(),
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": MCP_PROTOCOL_VERSION,
                            "capabilities": {},
                            "clientInfo": { "name": "legacy-http", "version": "1.0" }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(MCP_PROTOCOL_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(MCP_PROTOCOL_VERSION)
    );
    assert!(
        response.headers().get(MCP_SESSION_HEADER).is_some(),
        "legacy initialize must mint a session id"
    );
}
