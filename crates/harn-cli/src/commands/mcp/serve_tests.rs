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
#[path = "serve_tests/run_report.rs"]
mod run_report;

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

pub fn on_ok(harness: Harness, event: TriggerEvent) -> dict {
  harness.stdio.log("ok:" + event.kind)
  return {kind: event.kind, event_id: event.id, trace_id: event.trace_id}
}

pub fn on_fail(harness: Harness, event: TriggerEvent) -> any {
  throw "boom:" + event.kind
}
"#,
    );
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
                    "_meta": stable_meta(),
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
                "params": { "uri": uri, "_meta": stable_meta() }
            }),
        )
        .await;
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    serde_json::from_str(text).unwrap_or_else(|_| json!(text))
}

#[tokio::test(flavor = "current_thread")]
async fn resource_template_and_empty_prompt_lists_roundtrip() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

    let templates = service
        .handle_request(
            &mut session,
            stable_request(10, "resources/templates/list", json!({})),
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
            stable_request(
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
        .handle_request(&mut session, stable_request(11, "prompts/list", json!({})))
        .await;
    assert_eq!(prompts["result"]["prompts"], json!([]));

    let prompt = service
        .handle_request(
            &mut session,
            stable_request(12, "prompts/get", json!({"name": "missing"})),
        )
        .await;
    assert_eq!(prompt["error"]["code"], json!(-32602));
}

#[tokio::test(flavor = "current_thread")]
async fn file_backed_prompts_list_render_and_refresh_changes() {
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
    let mut session = ConnectionState::default();

    let prompts = service
        .handle_request(&mut session, stable_request(20, "prompts/list", json!({})))
        .await;
    assert_eq!(prompts["result"]["prompts"][0]["name"], json!("review"));
    assert_eq!(
        prompts["result"]["prompts"][0]["arguments"][0]["description"],
        json!("Code to review")
    );

    let completion = service
        .handle_request(
            &mut session,
            stable_request(
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
            stable_request(21, "prompts/get", json!({"name": "review"})),
        )
        .await;
    assert_eq!(missing["error"]["code"], json!(-32602));

    let prompt = service
        .handle_request(
            &mut session,
            stable_request(
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
    let updated = service
        .handle_request(
            &mut session,
            stable_request(
                23,
                "prompts/get",
                json!({"name": "review", "arguments": {"code": "changed"}}),
            ),
        )
        .await;
    assert_eq!(
        updated["result"]["messages"][0]["content"]["text"],
        json!("Updated: changed\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tools_list_advertises_tool_metadata_per_tool() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

    let response = service
        .handle_request(&mut session, stable_request(30, "tools/list", json!({})))
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
    assert!(trigger_fire.get("execution").is_none());
    assert_eq!(trigger_fire["annotations"]["readOnlyHint"], json!(false));
    assert_eq!(trigger_fire["annotations"]["destructiveHint"], json!(true));
    assert_eq!(trigger_fire["annotations"]["openWorldHint"], json!(true));
    assert!(trigger_list.get("execution").is_none());
    assert_eq!(trigger_list["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(trigger_list["annotations"]["idempotentHint"], json!(true));
    assert_eq!(trigger_list["annotations"]["openWorldHint"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn inline_tool_completes_when_client_supports_tasks() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

    let response = service
        .handle_request(
            &mut session,
            stable_request(
                100,
                "tools/call",
                json!({
                    "name": "harn.trigger.list",
                    "arguments": {},
                    "_meta": {
                        mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES: {
                            "extensions": {mcp_protocol::TASKS_EXTENSION_ID: {}}
                        }
                    }
                }),
            ),
        )
        .await;
    assert_eq!(response["result"]["isError"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_fire_task_roundtrip_polls_and_retrieves_result() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

    let created = service
        .handle_request(
            &mut session,
            stable_request(
                101,
                "tools/call",
                json!({
                    "name": "harn.trigger.fire",
                    "arguments": {
                        "trigger_id": "cron-ok",
                        "payload": {}
                    },
                    "_meta": {
                        mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES: {
                            "extensions": {mcp_protocol::TASKS_EXTENSION_ID: {}}
                        }
                    }
                }),
            ),
        )
        .await;
    assert_eq!(created["result"]["resultType"], json!("task"));
    assert_eq!(created["result"]["status"], json!("working"));
    assert_eq!(created["result"]["ttlMs"], json!(DEFAULT_TASK_TTL_MS));
    let task_id = created["result"]["taskId"].as_str().unwrap();

    let notify = service
        .tasks
        .lock()
        .expect("MCP tasks poisoned")
        .get(task_id)
        .expect("created task")
        .notify
        .clone();
    let task = loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let task = service
            .handle_request(
                &mut session,
                stable_request(102, "tasks/get", json!({ "taskId": task_id })),
            )
            .await;
        if task["result"]["status"] == json!("completed") {
            break task;
        }
        notified.await;
    };
    assert_eq!(task["result"]["status"], json!("completed"));
    assert_eq!(task["result"]["result"]["isError"], json!(false));
    assert_eq!(
        task["result"]["result"]["structuredContent"]["status"],
        json!("dispatched")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_list_tool_returns_manifest_bindings() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

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
    let mut session = ConnectionState::default();

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
    let mut session = ConnectionState::default();

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
        "stable-client/1.0"
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
async fn topic_resource_reflects_event_log_changes() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

    let fire = call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({ "trigger_id": "cron-ok", "payload": {} }),
    )
    .await;
    assert_eq!(fire["status"], "dispatched");

    let topic = read_resource(&service, &mut session, "harn://topic/trigger.outbox").await;
    assert_eq!(topic["topic"], json!("trigger.outbox"));
    assert!(topic["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_replay_tool_replays_event() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();
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
    let mut session = ConnectionState::default();

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
    let mut session = ConnectionState::default();

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
async fn trust_query_returns_filtered_trace_groups() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = ConnectionState::default();

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
    let mut session = ConnectionState::default();

    let manifest = read_resource(&service, &mut session, "harn://manifest").await;
    let manifest = manifest.as_str().unwrap();
    assert!(manifest.contains("[[triggers]]"));
    assert!(manifest.contains("cron-ok"));
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
    let router = http_router_for_local(args.local.clone(), "/mcp".to_string()).unwrap();

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
                    stable_request(1, mcp_protocol::METHOD_SERVER_DISCOVER, json!({})).to_string(),
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
    let router = http_router_for_local(args.local.clone(), "/mcp".to_string()).unwrap();

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
    let router = http_router_for_local(args.local.clone(), "/mcp".to_string()).unwrap();

    let discover_body =
        Body::from(stable_request(1, mcp_protocol::METHOD_SERVER_DISCOVER, json!({})).to_string());
    let valid = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer valid-token")
                .body(discover_body)
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
                        stable_request(1, mcp_protocol::METHOD_SERVER_DISCOVER, json!({}))
                            .to_string(),
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
                    stable_request(1, mcp_protocol::METHOD_SERVER_DISCOVER, json!({})).to_string(),
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

fn stable_meta() -> JsonValue {
    json!({
        mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION: mcp_protocol::PROTOCOL_VERSION,
        mcp_protocol::MCP_META_KEY_CLIENT_INFO: {"name": "stable-client", "version": "1.0"},
        mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES: {},
    })
}

fn stable_request(id: i64, method: &str, params: JsonValue) -> JsonValue {
    let mut params = params.as_object().cloned().unwrap_or_default();
    let meta = params
        .entry("_meta".to_string())
        .or_insert_with(|| json!({}));
    let meta = meta.as_object_mut().expect("_meta must be an object");
    for (key, value) in stable_meta().as_object().expect("stable meta") {
        meta.entry(key.clone()).or_insert_with(|| value.clone());
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn server_discover_returns_stable_capabilities() {
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
                "params": { "_meta": stable_meta() }
            }),
        )
        .await;
    assert_eq!(
        response["result"]["resultType"],
        json!(mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(response["result"]["ttlMs"], json!(0));
    assert_eq!(response["result"]["cacheScope"], json!("private"));
    let supported = response["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions array");
    assert_eq!(supported.as_slice(), [json!(MCP_PROTOCOL_VERSION)]);
    assert_eq!(response["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(response["result"]["capabilities"]["resources"], json!({}));
    assert_eq!(response["result"]["capabilities"]["prompts"], json!({}));
    assert_eq!(
        response["result"]["capabilities"]["extensions"][mcp_protocol::TASKS_EXTENSION_ID],
        json!({})
    );
    assert!(session.authenticated);
}

#[tokio::test(flavor = "current_thread")]
async fn stable_tools_list_requires_only_request_metadata() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = McpOrchestratorService::new_local(args.local.clone()).unwrap();
    // Stable requests are self-describing and need no connection-scoped handshake.
    let mut session = ConnectionState::default();
    let response = service
        .handle_request(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": { "_meta": stable_meta() }
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
    assert!(session.authenticated);
}

#[tokio::test(flavor = "current_thread")]
async fn stable_request_with_unsupported_protocol_version_returns_minus_32022() {
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
                        mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION: "2099-01-01"
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
        .any(|v| v == &json!(mcp_protocol::PROTOCOL_VERSION)));
}

#[tokio::test(flavor = "current_thread")]
async fn http_stable_request_uses_stateless_stable_version() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(service.clone(), "/mcp".to_string());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(MCP_PROTOCOL_HEADER, mcp_protocol::PROTOCOL_VERSION)
                .header(mcp_protocol::MCP_HEADER_METHOD, "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/list",
                        "params": { "_meta": stable_meta() }
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
        Some(mcp_protocol::PROTOCOL_VERSION),
        "stable responses must echo the protocol version"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: JsonValue = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body["result"]["resultType"],
        json!(mcp_protocol::RESULT_TYPE_COMPLETE)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_stable_request_rejects_method_header_mismatch() {
    let _guard = lock_harn_state_async().await;
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    let args = fixture_args(&temp);
    let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
    let router = http_router_for_service(service.clone(), "/mcp".to_string());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .header(MCP_PROTOCOL_HEADER, mcp_protocol::PROTOCOL_VERSION)
                .header(mcp_protocol::MCP_HEADER_METHOD, "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "tools/call",
                        "params": { "name": "harn.trigger.list", "_meta": stable_meta() }
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
    assert_eq!(body["error"]["code"], json!(-32020));
    assert_eq!(body["error"]["data"]["headerValue"], "tools/list");
    assert_eq!(body["error"]["data"]["bodyMethod"], "tools/call");
}
