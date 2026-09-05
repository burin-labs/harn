use super::*;
use axum::body::{to_bytes, Body};
use axum::http::Request;
use tower::ServiceExt;
mod live_controls;
mod served_agent_turn;
mod session_model_policy;
mod task_cancellation;

fn write_test_pipeline(path: &Path) {
    std::fs::write(
        path,
        "pipeline main(harness: Harness) { harness.stdio.println(prompt) }\n",
    )
    .expect("write script");
}

#[tokio::test]
async fn openapi_json_is_served_from_canonical_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let response = api_router(server.state)
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["openapi"], "3.1.0");
    assert!(value["paths"]["/v1/sessions"].is_object());
}

#[tokio::test]
async fn local_api_registers_and_downloads_file_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let report = dir.path().join("report.pdf");
    std::fs::write(&report, b"%PDF-1.7\n").expect("write report");
    let report_uri = url::Url::from_file_path(&report)
        .expect("file url")
        .to_string();
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let app = api_router(server.state);

    let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/artifacts")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "kind": "file",
                            "mime_type": "application/pdf",
                            "uri": report_uri,
                            "visibility": "public",
                            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                            "name": "report.pdf",
                            "size_bytes": 9
                        }))
                        .expect("artifact json"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("artifact response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let artifact: Value = serde_json::from_slice(&body).expect("artifact");
    let artifact_id = artifact["id"].as_str().expect("artifact id");
    assert_eq!(artifact["object"], "artifact");
    assert_eq!(artifact["kind"], "file");
    assert_eq!(artifact["mime_type"], "application/pdf");
    assert_eq!(artifact["name"], "report.pdf");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/artifacts")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("list response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let list: Value = serde_json::from_slice(&body).expect("list");
    assert_eq!(list["data"].as_array().expect("data").len(), 1);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/artifacts/{artifact_id}/content"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("content response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/pdf")
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(&body[..], b"%PDF-1.7\n");
}

#[tokio::test]
async fn local_api_indexes_harn_artifact_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let state = server.state;

    state.register_session_update(json!({
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "artifact",
                    "_meta": {
                        "harn": {
                            "artifactId": "artifact-file",
                            "kind": "file",
                            "title": "Report PDF",
                            "mimeType": "application/pdf",
                            "spec": {
                                "uri": "file:///tmp/report.pdf",
                                "name": "report.pdf",
                                "mime_type": "application/pdf",
                                "size_bytes": 1234,
                                "sha256": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                            },
                            "fallback": "File artifact: report.pdf"
                        }
                    }
                }
            }
        }));

    let inner = state.inner.lock().expect("api state poisoned");
    let artifact = inner.artifacts.get("artifact-file").expect("artifact");
    assert_eq!(artifact["kind"], "file");
    assert_eq!(artifact["mime_type"], "application/pdf");
    assert_eq!(artifact["name"], "report.pdf");
    assert_eq!(
        artifact["sha256"],
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    );
    assert_eq!(artifact["metadata"]["harn_kind"], "file");
    assert!(inner
        .events
        .iter()
        .any(|event| event.event == "artifact.created"));
}

#[tokio::test]
async fn local_api_returns_session_view() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let app = api_router(server.state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace_id":"local"}"#))
                .expect("request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let session: Value = serde_json::from_slice(&body).expect("session");
    let session_id = session["id"].as_str().expect("session id");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/sessions/{session_id}/view"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("view response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let view: Value = serde_json::from_slice(&body).expect("view");
    assert_eq!(view["schema"], "harn.session_view.v1");
    assert_eq!(view["session"]["session_id"], session_id);
    assert_eq!(view["session"]["last_event_id"], 1);
    assert_eq!(view["metadata"]["event_count"], 1);
}

#[tokio::test]
async fn local_api_truncates_session_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let app = api_router(server.state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace_id":"local"}"#))
                .expect("request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let session: Value = serde_json::from_slice(&body).expect("session");
    let session_id = session["id"].as_str().expect("session id");

    for text in ["alpha", "beta"] {
        let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/v1/sessions/{session_id}/messages"))
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"role":"user","parts":[{{"type":"text","text":"{text}","visibility":"public"}}]}}"#
                        )))
                        .expect("request"),
                )
                .await
                .expect("message response");
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/truncate"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"keep_first":1,"reason":"user_edit"}"#))
                .expect("request"),
        )
        .await
        .expect("truncate response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let result: Value = serde_json::from_slice(&body).expect("truncate json");
    assert_eq!(result["object"], "session.truncate_result");
    assert_eq!(result["session_id"], session_id);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sessions/{session_id}/messages"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("messages response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let messages: Value = serde_json::from_slice(&body).expect("messages json");
    assert_eq!(messages["data"].as_array().expect("messages").len(), 1);
    assert_eq!(messages["data"][0]["parts"][0]["text"], "alpha");
}

#[tokio::test]
async fn authenticated_api_rejects_missing_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let config = ApiServerConfig::for_pipeline(script.to_string_lossy().to_string())
        .with_auth_policy(AuthPolicy {
            methods: vec![crate::auth::AuthMethodConfig::ApiKey(
                crate::auth::ApiKeyAuthConfig::single("secret"),
            )],
            mcp_allowlist: None,
        });
    let server = ApiServer::new(config);
    let response = api_router(server.state)
        .oneshot(
            Request::builder()
                .uri("/v1/sessions")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn workflow_trigger_runs_endpoint_projects_dispatch_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let event_log = server.state.event_log.as_ref().expect("event log").clone();
    let outbox_topic = Topic::new(harn_vm::TRIGGER_OUTBOX_TOPIC).expect("outbox topic");
    let action_graph_topic = Topic::new(ACTION_GRAPH_TOPIC).expect("action graph topic");

    let mut dispatch_headers = BTreeMap::new();
    dispatch_headers.insert("trigger_id".to_string(), "github.comment".to_string());
    dispatch_headers.insert("event_id".to_string(), "evt-123".to_string());
    dispatch_headers.insert("binding_key".to_string(), "github-comment".to_string());
    dispatch_headers.insert("attempt".to_string(), "2".to_string());

    event_log
        .append(
            &outbox_topic,
            LogEvent {
                kind: "dispatch_succeeded".to_string(),
                payload: json!({
                    "handler_kind": "workflow",
                    "target_uri": "harn://workflows/comment_triage",
                    "result": {"session_id": "session-123"}
                }),
                headers: dispatch_headers,
                occurred_at_ms: 2_000,
            },
        )
        .await
        .expect("append dispatch");
    event_log
        .append(
            &outbox_topic,
            LogEvent {
                kind: "diagnostic".to_string(),
                payload: json!({}),
                headers: BTreeMap::new(),
                occurred_at_ms: 2_001,
            },
        )
        .await
        .expect("append ignored event");

    let mut graph_headers = BTreeMap::new();
    graph_headers.insert("event_id".to_string(), "evt-123".to_string());
    event_log
        .append(
            &action_graph_topic,
            LogEvent {
                kind: "action_graph_observed".to_string(),
                payload: json!({
                    "observability": {
                        "action_graph_nodes": [{"id": "trigger", "label": "GitHub comment"}],
                        "action_graph_edges": [{"from": "trigger", "to": "workflow"}]
                    }
                }),
                headers: graph_headers,
                occurred_at_ms: 2_002,
            },
        )
        .await
        .expect("append graph");

    let response = api_router(server.state)
        .oneshot(
            Request::builder()
                .uri("/v1/workflow-trigger-runs?limit=1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let data = body["data"].as_array().expect("data");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["object"], "workflow_trigger_run");
    assert_eq!(data[0]["status"], "succeeded");
    assert_eq!(data[0]["trigger_id"], "github.comment");
    assert_eq!(data[0]["event_id"], "evt-123");
    assert_eq!(data[0]["binding_key"], "github-comment");
    assert_eq!(data[0]["attempt"], 2);
    assert_eq!(data[0]["handler_kind"], "workflow");
    assert_eq!(data[0]["target_uri"], "harn://workflows/comment_triage");
    assert_eq!(data[0]["result"]["session_id"], "session-123");
    assert_eq!(data[0]["action_graph"]["nodes"][0]["id"], "trigger");
}

async fn build_test_router() -> axum::Router {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    // Leak the tempdir so the workspace_root stays alive for the
    // lifetime of the test; the router holds a path-only reference
    // and we don't need to clean up the on-disk artifact.
    std::mem::forget(dir);
    api_router(server.state)
}

async fn read_json(response: Response) -> Value {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&body).expect("json")
}

#[tokio::test]
async fn provider_catalog_endpoint_matches_export_artifact_with_overrides() {
    let _reset = crate::test_support::LlmOverrideReset;
    let overlay = crate::test_support::fixture_provider_overlay();
    let capability_overlay = crate::test_support::fixture_capability_overlay();
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().to_string());
    config.acp = config
        .acp
        .with_llm_overrides(Some(overlay.clone()), Some(capability_overlay.clone()));
    let server = ApiServer::new(config);

    let response = api_router(server.state)
        .oneshot(
            Request::builder()
                .uri("/v1/provider-catalog")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let expected = serde_json::to_value(harn_vm::provider_catalog::artifact_with_overrides(
        Some(&overlay),
        Some(&capability_overlay),
    ))
    .expect("expected catalog json");
    assert_eq!(body, expected);

    let providers = body["providers"].as_array().expect("providers");
    let provider = providers
        .iter()
        .find(|provider| provider["id"] == "fixture_runtime")
        .expect("fixture provider");
    assert_eq!(provider["classification"], "hosted");
    assert_eq!(
        provider["auth"],
        json!({
            "style": "bearer",
            "env": ["FIXTURE_RUNTIME_API_KEY"],
            "required": true
        })
    );

    let models = body["models"].as_array().expect("models");
    let model = models
        .iter()
        .find(|model| model["id"] == "fixture-model-v1")
        .expect("fixture model");
    assert_eq!(model["context_window"], 12345);
    assert_eq!(model["pricing"]["input_per_mtok"], 1.25);
    assert_eq!(model["aliases"], json!(["fixture-default"]));
    assert_eq!(model["tool_support"]["native"], true);
    assert_eq!(model["tool_support"]["tool_search"], json!(["hosted"]));
    assert_eq!(
        model["capability_tags"],
        json!([
            "streaming",
            "tools",
            "tool_search",
            "vision",
            "prompt_caching",
            "thinking",
            "extended_thinking",
            "structured_output"
        ])
    );
}

#[tokio::test]
async fn permissions_policy_installs_and_lints() {
    let app = build_test_router().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/permissions/policy")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"read":["src/**"],"escalate_to":["user"]}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["object"], "permission_policy");
    let version = body["version"].as_str().expect("version");
    assert!(version.starts_with("policy-"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/permissions/policy")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["policy"]["read"][0], "src/**");

    // Linter rejects empty patterns.
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/permissions/policy")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"read":[""]}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn permission_rules_round_trip_and_check_uses_them() {
    let app = build_test_router().await;
    let rule = RememberRule::new(
        DecisionScope::Session,
        Some("s1".to_string()),
        ActionClass::Read,
        "fs.*",
        "src/**",
        true,
        "alice",
    )
    .expect("rule compiles");
    let rule_body = serde_json::to_string(&rule).expect("rule json");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/permissions/rules")
                .header("content-type", "application/json")
                .body(Body::from(rule_body))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = read_json(response).await;
    assert_eq!(status, StatusCode::OK, "create rule failed: {body}");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/permissions/rules")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["data"].as_array().expect("rules").len(), 1);

    let check_request = PermissionRequest::new(
        "p1",
        "s1",
        "alice",
        ActionClass::Read,
        "fs.read",
        "src/lib.rs",
    );
    let check_body = serde_json::to_string(&check_request).expect("request json");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/permissions/check")
                .header("content-type", "application/json")
                .body(Body::from(check_body))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = read_json(response).await;
    assert_eq!(status, StatusCode::OK, "check failed: {body}");
    assert_eq!(body["decision"]["outcome"], "granted");
    assert_eq!(body["decision"]["scope"], "session");

    let history = app
        .oneshot(
            Request::builder()
                .uri("/v1/permissions/history?session_id=s1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = read_json(history).await;
    assert_eq!(body["data"].as_array().expect("history").len(), 1);
}

#[tokio::test]
async fn permission_check_returns_suspend_when_no_rule_or_policy() {
    let app = build_test_router().await;
    let request = PermissionRequest::new(
        "p1",
        "s1",
        "alice",
        ActionClass::Exec,
        "shell.exec",
        "rm -rf /",
    );
    let body = serde_json::to_string(&request).expect("request json");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/permissions/check")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = read_json(response).await;
    assert_eq!(status, StatusCode::OK, "check failed: {body}");
    assert_eq!(body["decision"]["outcome"], "suspend");
}
