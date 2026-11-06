use super::*;
mod data;
mod listing;
mod references;

fn upstream_schema(name: &str) -> jsonschema::Validator {
    let mut schema: JsonValue = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/schemas/a2a-1.0.1.schema.json"
    )))
    .unwrap();
    schema["$ref"] = json!(format!("#/$defs/lf.a2a.v1.{name}.jsonschema.json"));
    jsonschema::validator_for(&schema).expect("upstream schema has no unresolved references")
}

fn assert_upstream(name: &str, value: &JsonValue) {
    let schema = upstream_schema(name);
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:?}\n{value}");
}

fn legacy_profile_schema(name: &str) -> jsonschema::Validator {
    let mut schema: JsonValue = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/schemas/a2a-0.3.0.schema.json"
    )))
    .unwrap();
    schema.as_object_mut().unwrap().remove("anyOf");
    schema["$ref"] = json!(format!("#/$defs/{name}"));
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_legacy_profile(name: &str, value: &JsonValue) {
    let validator = legacy_profile_schema(name);
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:?}\n{value}");
}

// These fixtures emit status and reply events. Validate their exact profile
// shapes, avoiding the generic result union's overlapping push-config branch.
fn legacy_fixture_stream_valid(value: &JsonValue) -> bool {
    let event = &value["result"];
    legacy_profile_schema("StreamEventResponse").is_valid(value)
        && value.get("error").is_none()
        && value
            .get("id")
            .is_some_and(|id| legacy_profile_schema("RequestId").is_valid(id))
        && event
            .get("contextId")
            .is_none_or(|context| context.is_null() || context.is_string())
        && !["task", "statusUpdate", "artifactUpdate"]
            .iter()
            .any(|key| event.get(key).is_some())
        && match event["kind"].as_str() {
            Some("status-update") => event["type"] == "status" && event["final"].is_boolean(),
            Some("message") => {
                event["type"] == "message"
                    && legacy_profile_schema("Message").is_valid(&event["message"])
                    && event["message"]["messageId"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
            }
            _ => false,
        }
}

fn send_params() -> JsonValue {
    json!({"message": {"messageId": "client-message", "role": "ROLE_USER", "parts": [{"text": "hello"}]}})
}

async fn rpc(
    router: Router,
    version: &str,
    method: &str,
    params: JsonValue,
) -> (StatusCode, JsonValue) {
    rpc_as(router, version, method, params, None).await
}

async fn rpc_as(
    router: Router,
    version: &str,
    method: &str,
    params: JsonValue,
    key: Option<&str>,
) -> (StatusCode, JsonValue) {
    let request = harn_vm::jsonrpc::request("request", method, params);
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/")
        .header(A2A_VERSION_HEADER, version);
    if let Some(key) = key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let response = router
        .clone()
        .oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let result =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    (status, result)
}

#[tokio::test]
async fn v1_discovery_advertises_versioned_interfaces() {
    let (_dir, server) = echo_server();
    let router = http_router(server, LOCAL_URL);
    let response = router
        .oneshot(
            Request::builder()
                .uri(A2A_AGENT_CARD_PATH)
                .header(A2A_VERSION_HEADER, "1.0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let card: JsonValue =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let interfaces = card["supportedInterfaces"]
        .as_array()
        .expect("1.0 interfaces");
    assert_upstream("AgentCard", &card);
    let mut legacy = card.clone();
    legacy["protocolVersion"] = json!("0.3.0");
    assert!(!upstream_schema("AgentCard").is_valid(&legacy));
    assert!(interfaces
        .iter()
        .any(|entry| entry["protocolBinding"] == "JSONRPC" && entry["protocolVersion"] == "1.0"));
    assert!(interfaces
        .iter()
        .any(|entry| entry["protocolBinding"] == "JSONRPC" && entry["protocolVersion"] == "0.3.0"));
}

#[tokio::test]
async fn v1_send_dispatches_and_returns_a_versioned_task() {
    let (_dir, server) = echo_server();
    let router = http_router(server, LOCAL_URL);
    let body = harn_vm::jsonrpc::request(
        "v1-send",
        "SendMessage",
        json!({
            "message": {
                "messageId": "message-1",
                "contextId": "context-1",
                "role": "ROLE_USER",
                "parts": [{"text": "hello"}],
                "metadata": {"target_agent": "triage"}
            }
        }),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(A2A_VERSION_HEADER, "1.0")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: JsonValue =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(
        result["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
        "{result}"
    );
    assert_eq!(
        result["result"]["task"]["contextId"], "context-1",
        "{result}"
    );
    assert!(result["result"]["task"]["id"].is_string(), "{result}");
    let task = &result["result"]["task"];
    assert_upstream("SendMessageResponse", &result["result"]);
    let mut wrong = result["result"].clone();
    wrong["task"]["status"]["state"] = json!("completed");
    assert!(!upstream_schema("SendMessageResponse").is_valid(&wrong));
    assert_eq!(task["history"][0]["messageId"], "message-1");
    assert_eq!(task["history"][1]["role"], "ROLE_AGENT");
    assert_eq!(task["history"][1]["parts"][0]["text"], "hello");
    assert!(task["history"][1]["parts"][0].get("type").is_none());
}

#[tokio::test]
async fn v1_get_limits_history_and_cancel_reaches_live_task() {
    let (_dir, server) = echo_server();
    let router = http_router(server.clone(), LOCAL_URL);
    let (_, sent) = rpc(router.clone(), "1.0", "SendMessage", send_params()).await;
    let id = &sent["result"]["task"]["id"];
    assert!(id.is_string(), "{sent}");
    let (_, got) = rpc(
        router.clone(),
        "1.0",
        "GetTask",
        json!({"id": id, "historyLength": 0}),
    )
    .await;
    assert_eq!(got["result"]["id"], *id);
    assert_upstream("Task", &got["result"]);
    assert_eq!(got["result"]["history"], json!([]));
    assert_eq!(got["result"]["status"]["state"], "TASK_STATE_COMPLETED");

    let pending = server
        .prepare_task(
            &json!({"message":{"parts":[{"text":"pending"}]}, "contextId":"cancel-context"}),
            AuthRequest::default(),
            None,
        )
        .await
        .unwrap_or_else(|error| {
            panic!(
                "prepare cancellable task: {}",
                error.with_id(json!("prepare"))
            )
        });
    assert!(!pending.cancel_token.load(Ordering::SeqCst));
    let subscribe = harn_vm::jsonrpc::request(
        "active-subscription",
        "SubscribeToTask",
        json!({"id": pending.id}),
    );
    let subscription = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(A2A_VERSION_HEADER, "1.0")
                .body(Body::from(serde_json::to_vec(&subscribe).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(subscription.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let (_, cancelled) = rpc(
        router.clone(),
        "1.0",
        "CancelTask",
        json!({"id": pending.id}),
    )
    .await;
    assert_eq!(
        cancelled["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "{cancelled}"
    );
    assert!(pending.cancel_token.load(Ordering::SeqCst));
    assert_upstream("Task", &cancelled["result"]);
    let stream = String::from_utf8(
        to_bytes(subscription.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(stream.contains("TASK_STATE_CANCELED"), "{stream}");
    let (_, repeated) = rpc(router, "1.0", "CancelTask", json!({"id": pending.id})).await;
    assert_eq!(repeated["error"]["code"], A2A_TASK_NOT_CANCELABLE);
}

#[tokio::test]
async fn v1_stream_uses_response_unions_and_refuses_terminal_subscriptions() {
    let (_dir, server) = echo_server();
    let router = http_router(server, LOCAL_URL);
    let request =
        harn_vm::jsonrpc::request("stream-request", "SendStreamingMessage", send_params());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(A2A_VERSION_HEADER, "1.0")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let frames: Vec<JsonValue> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!frames.is_empty(), "{text}");
    assert!(frames[0]["result"]["task"]["id"].is_string(), "{text}");
    assert_eq!(
        frames[0]["result"]["task"]["status"]["state"],
        "TASK_STATE_SUBMITTED"
    );
    for frame in &frames {
        assert_eq!(frame["id"], "stream-request");
        assert_upstream("StreamResponse", &frame["result"]);
    }
    let status = frames.last().unwrap();
    assert_eq!(
        status["result"]["statusUpdate"]["status"]["state"], "TASK_STATE_COMPLETED",
        "{text}"
    );
    assert!(
        frames.iter().any(
            |frame| frame["result"]["statusUpdate"]["status"]["message"]["parts"][0]["text"]
                == "hello"
        ),
        "{text}"
    );
    let task_id = &status["result"]["statusUpdate"]["taskId"];
    let request =
        harn_vm::jsonrpc::request("resubscribe", "SubscribeToTask", json!({"id": task_id}));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(A2A_VERSION_HEADER, "1.0")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let refused: JsonValue =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(refused["id"], "resubscribe");
    assert_eq!(refused["error"]["code"], -32004);
}

#[tokio::test]
async fn v1_refuses_mismatched_messages_before_task_creation() {
    let (_dir, server) = echo_server();
    let router = http_router(server.clone(), LOCAL_URL);
    for (request, expected) in [
        (
            json!({"jsonrpc":"2.0","id":"shape","params":send_params()}),
            -32600,
        ),
        (
            json!({"jsonrpc":"2.0","id":"shape","method":42,"params":send_params()}),
            -32600,
        ),
        (
            json!({"jsonrpc":"2.0","id":"shape","method":"UnknownMethod","params":send_params()}),
            -32601,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(A2A_VERSION_HEADER, "1.0")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let rejected: JsonValue =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(rejected["error"]["code"], expected, "{rejected}");
        assert_eq!(
            rejected["id"],
            if expected == -32600 {
                JsonValue::Null
            } else {
                json!("shape")
            }
        );
        assert!(server.tasks.lock().unwrap().is_empty());
    }
    for message in [
        json!({}),
        json!({"messageId":"", "role":"ROLE_USER", "parts":[{"text":"hello"}]}),
        json!({"messageId":"a", "role":"ROLE_USER", "parts":[]}),
        json!({"messageId":"a", "role":"user", "parts":[{"text":"hello"}]}),
        json!({"messageId":"a", "role":"ROLE_USER", "parts":[{"text":"hello", "data":{}}]}),
        json!({"messageId":"a", "role":"ROLE_USER", "parts":[{"raw":"not base64"}]}),
        json!({"messageId":"a", "role":"ROLE_USER", "parts":[{"kind":"text", "text":"hello"}]}),
        json!({"messageId":"a", "role":"ROLE_USER", "parts":[{"text":"hello"}], "contextId":1}),
    ] {
        let (_, result) = rpc(
            router.clone(),
            "1.0",
            "SendMessage",
            json!({"message":message}),
        )
        .await;
        assert_eq!(result["error"]["code"], -32602, "{result}");
        assert!(server.tasks.lock().unwrap().is_empty());
    }
    for (version, method) in [("", "SendMessage"), ("1.0", "message/send")] {
        let (_, result) = rpc(router.clone(), version, method, send_params()).await;
        assert!(result.get("error").is_some(), "{result}");
        assert!(server.tasks.lock().unwrap().is_empty());
    }
    let mut invalid_configuration = send_params();
    invalid_configuration["configuration"] = json!({"returnImmediately":"false"});
    let (_, result) = rpc(router, "1.0", "SendMessage", invalid_configuration).await;
    assert_eq!(result["error"]["code"], -32602);
    assert!(server.tasks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn v1_file_parts_round_trip_through_shared_task_storage() {
    let (_dir, server) = echo_server();
    let mut params = send_params();
    params["message"]["parts"] = json!([{"text":"hello"}, {"raw":"aGVsbG8=", "filename":"note.txt", "mediaType":"text/plain"}, {"data":{"value":1}}]);
    let router = http_router(server, LOCAL_URL);
    let (_, result) = rpc(router.clone(), "1.0", "SendMessage", params).await;
    assert!(result.get("error").is_none(), "{result}");
    assert_upstream("SendMessageResponse", &result["result"]);
    let part = &result["result"]["task"]["history"][0]["parts"][1];
    assert_eq!(part["raw"], "aGVsbG8=");
    assert_eq!(part["filename"], "note.txt");
    assert_eq!(part["mediaType"], "text/plain");
    assert!(part.get("file").is_none());
    assert_eq!(
        result["result"]["task"]["history"][0]["parts"][2]["data"],
        json!({"value":1})
    );
    let (_, legacy) = rpc(
        router,
        "0.3",
        "tasks/get",
        json!({"id":result["result"]["task"]["id"]}),
    )
    .await;
    assert_legacy_profile("Task", &legacy["result"]);
    assert_eq!(
        legacy["result"]["history"][0]["parts"][2]["data"],
        json!({"value":1})
    );
}

#[tokio::test]
async fn both_versions_share_task_identity_with_distinct_wire_shapes() {
    let (_dir, server) = echo_server();
    let router = http_router(server, LOCAL_URL);
    let (_, legacy) = rpc(
        router.clone(),
        "0.3",
        "message/send",
        json!({"message":{"role":"user","parts":[{"kind":"text","text":"legacy"}]}}),
    )
    .await;
    assert_eq!(legacy["result"]["status"]["state"], "completed", "{legacy}");
    assert_legacy_profile("Task", &legacy["result"]);
    let mut malformed = legacy["result"].clone();
    malformed.as_object_mut().unwrap().remove("id");
    assert!(!legacy_profile_schema("Task").is_valid(&malformed));
    let id = &legacy["result"]["id"];
    let (_, modern) = rpc(router.clone(), "1.0", "GetTask", json!({"id":id})).await;
    assert_upstream("Task", &modern["result"]);
    assert_eq!(modern["result"]["id"], *id);
    assert_eq!(modern["result"]["contextId"], *id);
    assert_eq!(modern["result"]["history"][1]["parts"][0]["text"], "legacy");
    let (_, legacy_again) = rpc(router, "0.3", "tasks/get", json!({"id":id})).await;
    assert_eq!(legacy_again["result"], legacy["result"]);
}

#[tokio::test]
async fn legacy_discovery_and_stream_match_the_pinned_profile() {
    let (_dir, server) = echo_server();
    let router = http_router(server, LOCAL_URL);
    let card = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(A2A_AGENT_CARD_PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let card: JsonValue =
        serde_json::from_slice(&to_bytes(card.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_legacy_profile("AgentCard", &card);
    let request = harn_vm::jsonrpc::request(
        "legacy-stream",
        "message/stream",
        json!({"message":{"role":"user","parts":[{"kind":"text","text":"legacy stream"}]}}),
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(A2A_VERSION_HEADER, "0.3")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let text = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let frames: Vec<JsonValue> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!frames.is_empty(), "{text}");
    for frame in &frames {
        assert!(legacy_fixture_stream_valid(frame), "{frame}");
        assert_eq!(frame["id"], "legacy-stream");
    }
    for (path, value) in [
        ("/jsonrpc", json!("1.0")),
        ("/id", json!({})),
        ("/result/kind", json!("unknown-event")),
        ("/result/status/state", json!("unknown-state")),
    ] {
        let mut malformed = frames[0].clone();
        *malformed.pointer_mut(path).expect("actual emitted field") = value;
        assert!(!legacy_fixture_stream_valid(&malformed), "{malformed}");
    }
    let mut missing_id = frames[0].clone();
    missing_id.as_object_mut().unwrap().remove("id");
    assert!(!legacy_fixture_stream_valid(&missing_id));
    let mut invalid_context = frames[0].clone();
    invalid_context["result"]["contextId"] = json!(true);
    assert!(!legacy_fixture_stream_valid(&invalid_context));
    let modern_wrapper = json!({"jsonrpc":"2.0", "id":"legacy-stream", "result":{"statusUpdate":frames[0]["result"]}});
    assert!(!legacy_fixture_stream_valid(&modern_wrapper));
    let mut broken_message = frames
        .iter()
        .find(|frame| frame["result"]["kind"] == "message")
        .expect("actual reply event")
        .clone();
    broken_message["result"]["message"]["parts"] = json!([]);
    assert!(!legacy_fixture_stream_valid(&broken_message));
    assert_eq!(
        frames.last().unwrap()["result"]["status"]["state"],
        "completed"
    );
    assert!(frames
        .iter()
        .any(|frame| frame["result"]["parts"][0]["text"] == "legacy stream"));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/message:stream")
                .body(Body::from(serde_json::to_vec(&request["params"]).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let text = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let frames: Vec<JsonValue> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!frames.is_empty(), "{text}");
    for frame in &frames {
        assert!(
            frame.get("result").is_none(),
            "REST returned a JSON-RPC envelope: {frame}"
        );
        assert!(
            legacy_fixture_stream_valid(&json!({"jsonrpc":"2.0", "id":"rest", "result":frame})),
            "{frame}"
        );
    }
    assert_eq!(frames.last().unwrap()["status"]["state"], "completed");
}

#[tokio::test]
async fn both_versions_cancel_after_real_dispatch_progress() {
    const SOURCE: &str = include_str!("fixtures/cancel_after_progress.harn");
    for version in ["0.3", "1.0"] {
        let (_dir, server) = test_server(SOURCE);
        let router = http_router(server.clone(), LOCAL_URL);
        let modern = version == "1.0";
        let mut params = if modern {
            send_params()
        } else {
            json!({"message":{"role":"user","parts":[{"kind":"text","text":"cancel"}]}})
        };
        params["configuration"] = json!({"returnImmediately":true});
        let (_, sent) = rpc(
            router.clone(),
            version,
            if modern {
                "SendMessage"
            } else {
                "message/send"
            },
            params,
        )
        .await;
        let task = if modern {
            &sent["result"]["task"]
        } else {
            &sent["result"]
        };
        let id = task["id"].as_str().expect("published task id");
        let mut events = server.subscribe(id).expect("published task subscription");
        let mut reached = false;
        while let Some(event) = events.next().await {
            if event
                .pointer("/result/status/message/parts/0/text")
                .and_then(JsonValue::as_str)
                == Some("Ready for HTTP cancellation.")
            {
                reached = true;
                break;
            }
        }
        assert!(reached, "{version}: dispatch never reached progress");
        let (_, cancelled) = rpc(
            router,
            version,
            if modern { "CancelTask" } else { "tasks/cancel" },
            json!({"id":id}),
        )
        .await;
        assert_eq!(
            cancelled["result"]["status"]["state"],
            if modern {
                "TASK_STATE_CANCELED"
            } else {
                "canceled"
            },
            "{cancelled}"
        );
        assert!(server.is_cancelled(id));
        let wire_version = if modern {
            harn_vm::a2a::A2aWireVersion::V1_0
        } else {
            harn_vm::a2a::A2aWireVersion::V0_3
        };
        assert_eq!(
            wire_version
                .task_state(cancelled["result"]["status"]["state"].as_str().unwrap())
                .unwrap(),
            "canceled"
        );
        if modern {
            assert_upstream("Task", &cancelled["result"]);
        } else {
            assert_legacy_profile("Task", &cancelled["result"]);
        }
    }
}

#[tokio::test]
async fn v1_extended_card_preserves_authentication_and_uses_upstream_shapes() {
    let (_dir, server) = super::protocol::server_with_api_key_policy(ECHO_SCRIPT, "fixture-key");
    let router = http_router(server, LOCAL_URL);
    let (status, result) = rpc(router.clone(), "1.0", "GetExtendedAgentCard", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{result}");
    let request = harn_vm::jsonrpc::request("card", "GetExtendedAgentCard", json!({}));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(A2A_VERSION_HEADER, "1.0")
                .header("authorization", "Bearer fixture-key")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: JsonValue =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    assert_upstream("AgentCard", &result["result"]);
    assert_eq!(result["result"]["capabilities"]["extendedAgentCard"], true);
    assert_eq!(
        result["result"]["securityRequirements"][0]["schemes"]["apiKey"]["list"],
        json!([])
    );
}
