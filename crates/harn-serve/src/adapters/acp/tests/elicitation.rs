use super::*;
use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};

#[derive(Clone)]
struct McpInput {
    mode: &'static str,
    requests: mpsc::UnboundedSender<serde_json::Value>,
}

async fn mcp_input(
    State(input): State<McpInput>,
    Json(request): Json<serde_json::Value>,
) -> axum::response::Response {
    input.requests.send(request.clone()).unwrap();
    if request.get("id").is_none() {
        return axum::http::StatusCode::ACCEPTED.into_response();
    }
    let id = &request["id"];
    if request["method"] == "server/discover" {
        return Json(serde_json::json!({"jsonrpc":"2.0", "id":id,"result":{
            "resultType":"complete", "supportedVersions":["2026-07-28"],
            "capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"
        }}))
        .into_response();
    }
    assert_eq!(request["method"], "tools/call");
    if request["params"].get("inputResponses").is_none() {
        let mut params = if input.mode == "form" {
            serde_json::json!({"mode":"form","message":"Choose an answer","requestedSchema":{
                "type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]
            }})
        } else {
            serde_json::json!({"mode":"url","message":"Complete the external flow",
                "url":"https://example.com/authorize","elicitationId":"source-url"})
        };
        if input.mode == "url_no_id" {
            params.as_object_mut().unwrap().remove("elicitationId");
        }
        if input.mode == "invalid_mode" {
            params["mode"] = serde_json::json!(7);
        }
        return Json(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
            "resultType":"input_required", "inputRequests":{"answer":{"method":"elicitation/create","params":params}}
        }})).into_response();
    }
    let result = serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
        "resultType":"complete", "content":[{"type":"text","text":"elicitation reached MCP retry"}]
    }});
    if input.mode == "url" || input.mode == "url_no_id" {
        let unknown = serde_json::json!({"jsonrpc":"2.0","method":"notifications/elicitation/complete","params":{"elicitationId":"unknown"}});
        let complete = serde_json::json!({"jsonrpc":"2.0","method":"notifications/elicitation/complete","params":{"elicitationId":"source-url"}});
        return (
            [("content-type", "text/event-stream")],
            format!(
                "data: {unknown}\n\ndata: {complete}\n\ndata: {complete}\n\ndata: {result}\n\n"
            ),
        )
            .into_response();
    }
    Json(result).into_response()
}

fn upstream(definition: &str) -> jsonschema::Validator {
    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/schemas/acp-1.26.0-upstream.schema.json"
    )))
    .unwrap();
    jsonschema::draft202012::new(&serde_json::json!({
        "$ref":format!("#/$defs/{definition}"), "$defs":schema["$defs"]
    }))
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn optional_lifecycle_and_unstable_methods_remain_unadvertised_and_refused() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server.handle_initialize(&serde_json::json!(1), &serde_json::json!({}));
    let initialized = recv_json(&mut rx).await;
    let capabilities = &initialized["result"]["agentCapabilities"];
    assert!(capabilities["auth"].get("logout").is_none());
    assert!(capabilities["sessionCapabilities"].get("delete").is_none());
    for field in ["providers", "systemPrompt", "subagents", "sessionMessages"] {
        assert!(capabilities.get(field).is_none(), "{capabilities}");
    }
    assert!(initialized["result"]["authMethods"]
        .as_array()
        .unwrap()
        .iter()
        .all(|method| method["type"] == "agent"));
    for method in [
        "logout",
        "session/delete",
        "providers/list",
        "providers/set",
        "providers/disable",
        "mcp/message",
    ] {
        server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":2,"method":method,"params":{"sessionId":"unrelated"}})).await;
        let response = recv_response_with_id(&mut rx, 2).await;
        assert_eq!(response["error"]["code"], -32601, "{response}");
        assert!(server.sessions.is_empty());
    }
    let capabilities = super::super::client_capabilities::ClientCapabilities::default();
    for kind in [
        "state_update",
        "user_message",
        "session_redo",
        "session_rollback",
        "session_truncated",
    ] {
        assert!(!super::super::schema::is_standard_session_update(kind));
        assert!(!capabilities.supports_session_update(kind));
    }
    assert!(capabilities.supports_session_update("plan"));
}

#[tokio::test(flavor = "current_thread")]
async fn real_mcp_inputs_use_only_negotiated_elicitation_and_complete_known_urls_once() {
    harn_clock::test_support::with_local_tasks(async {
        for (capabilities, mode, action, supported, legacy) in [
            (serde_json::json!({"form":{}}), "form", "accept", true, false),
            (serde_json::json!({"form":{}}), "form", "decline", true, false),
            (serde_json::json!({"form":{}}), "form", "cancel", true, false),
            (serde_json::json!({"form":{}}), "form", "stop", true, false),
            (serde_json::json!({"form":{}}), "form", "bad_content", true, false),
            (serde_json::json!({"form":{}}), "form", "_future", true, false),
            (serde_json::json!({"url":{}}), "url", "accept", true, false),
            (serde_json::json!({"url":{}}), "url_no_id", "accept", true, false),
            (serde_json::json!({"url":{}}), "url", "decline", true, false),
            (serde_json::Value::Null, "form", "accept", true, true),
            (serde_json::json!({"form":{}}), "url", "accept", false, false),
            (serde_json::json!({}), "form", "accept", false, false),
            (serde_json::json!({"form":{}}), "invalid_mode", "accept", false, false),
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let pipeline = workspace.path().join("elicitation.harn");
            std::fs::write(&pipeline, include_str!("fixtures/elicitation.harn")).unwrap();
            let (record_tx, mut records) = mpsc::unbounded_channel();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let http = tokio::task::spawn_local(async move {
                axum::serve(listener, Router::new().route("/mcp", post(mcp_input)).with_state(McpInput { mode, requests: record_tx })).await.unwrap();
            });
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(run_acp_channel_server(AcpServerConfig::new(Some(pipeline.display().to_string())), request_rx, response_tx));
            request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":101,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{"elicitation":capabilities}}})).unwrap();
            assert!(recv_response_with_id(&mut response_rx, 101).await.get("result").is_some());
            request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":102,"method":"session/new","params":{"cwd":workspace.path(),"environmentPolicy":{"kind":"isolated"}}})).unwrap();
            let created = recv_response_with_id(&mut response_rx, 102).await;
            let session = created["result"]["sessionId"].as_str().unwrap();
            request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":105,"method":"session/set_mode","params":{"sessionId":session,"modeId":"code"}})).unwrap();
            assert!(recv_response_with_id(&mut response_rx, 105).await.get("result").is_some());
            request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":103,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":"collect"}]}})).unwrap();
            let answer = if action == "bad_content" {
                serde_json::json!({"action":"accept","content":{"answer":{"nested":"invalid"}}})
            } else if mode == "form" && action == "accept" {
                serde_json::json!({"action":action,"content":{"answer":"from ACP client"}})
            } else { serde_json::json!({"action":action}) };
            let mut elicited = 0;
            let mut completed = Vec::new();
            let mut url_id = None;
            let mut cancelled_request = None;
            let mut cancellation_seen = false;
            let terminal = loop {
                let message = recv_json(&mut response_rx).await;
                if message.get("method").is_none() && message["id"] == 103 { break message; }
                match message["method"].as_str() {
                    Some("elicitation/complete") => {
                        assert!(upstream("CompleteElicitationNotification").is_valid(&message["params"]));
                        completed.push(message["params"]["elicitationId"].clone());
                    }
                    Some("elicitation/create") => {
                        assert!(supported && !legacy, "unadvertised input reached client: {message}");
                        let validator = upstream("CreateElicitationRequest");
                        assert!(validator.is_valid(&message["params"]), "{message}");
                        let mut invalid = message["params"].clone(); invalid["message"] = serde_json::json!(7);
                        assert!(!validator.is_valid(&invalid));
                        assert_eq!(message["params"]["sessionId"], session);
                        assert_eq!(message["params"]["mode"], if mode == "url_no_id" { "url" } else { mode });
                        url_id = message["params"].get("elicitationId").cloned();
                        elicited += 1;
                        if action == "stop" {
                            cancelled_request = Some(message["id"].clone());
                            assert_ne!(message["id"], 103, "the two request directions must be distinguished");
                            request_tx.send(serde_json::json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":103}})).unwrap();
                            continue;
                        }
                        assert_eq!(upstream("CreateElicitationResponse").is_valid(&answer), action != "bad_content");
                        request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":answer})).unwrap();
                    }
                    Some("host/capabilities") => {
                        request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":{"project":["mcp_config"],"mcp":["elicit"]}})).unwrap();
                    }
                    Some("$/cancel_request") => {
                        assert!(upstream("CancelRequestNotification").is_valid(&message["params"]));
                        assert_eq!(message["params"]["requestId"], cancelled_request.clone().unwrap());
                        assert!(!cancellation_seen);
                        cancellation_seen = true;
                        request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":cancelled_request,"result":{"action":"accept","content":{"answer":"late"}}})).unwrap();
                    }
                    Some("host/call") if message["params"]["name"] == "project.mcp_config" => {
                        request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":[{"name":"probe","transport":"http","url":url}]})).unwrap();
                    }
                    Some("host/call") if message["params"]["name"] == "mcp.elicit" => {
                        assert!(legacy, "stable grants must use standard elicitation: {message}");
                        elicited += 1;
                        request_tx.send(serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":answer})).unwrap();
                    }
                    _ if message.get("id").is_some() => panic!("unexpected client request: {message}"),
                    _ => {}
                }
            };
            let mut calls = Vec::new();
            while let Ok(record) = records.try_recv() { if record["method"] == "tools/call" { calls.push(record); } }
            assert!(!calls.is_empty(), "the actual MCP input producer must run");
            if action == "stop" {
                assert_eq!(terminal["result"]["stopReason"], "cancelled", "{terminal}");
                assert_eq!(elicited, 1);
                assert!(cancellation_seen, "Stop must reach the remote elicitation owner");
                assert_eq!(calls.len(), 1, "a cancelled request must not retry on its late answer");
            } else if action == "bad_content" {
                assert!(terminal.get("error").is_some(), "{terminal}");
                assert_eq!(elicited, 1);
                assert_eq!(calls.len(), 1, "malformed input must not be forwarded to MCP");
            } else if supported {
                assert_eq!(terminal["result"]["stopReason"], "end_turn", "{terminal}");
                assert_eq!(elicited, 1);
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[1]["params"]["inputResponses"]["answer"], answer);
            } else {
                assert!(terminal.get("error").is_some(), "{terminal}");
                assert_eq!(elicited, 0);
                assert_eq!(calls.len(), 1, "unsupported input must not retry as accepted");
            }
            assert_eq!(completed, if mode == "url" && action == "accept" && supported { vec![url_id.unwrap()] } else { vec![] });
            drop(request_tx); server.await.unwrap(); http.abort();
        }
    }).await;
}
