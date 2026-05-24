use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode};
use futures::{SinkExt, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value as JsonValue};

use super::acp_hub::ACP_PATH;
use super::core::DEFAULT_MAX_BODY_BYTES;
use super::routes::{wait_for_test_release_file, PENDING_TOPIC};
use harn_vm::event_log::{
    install_default_for_base_dir, reset_active_event_log, AnyEventLog, EventLog, Topic,
};
use harn_vm::secrets::{
    RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
};
use harn_vm::{
    ProviderId, TriggerBindingSource, TriggerBindingSpec, TriggerHandlerSpec, TriggerRetryConfig,
};
use sha2::Sha256;
use tempfile::{tempdir, TempDir};

use crate::commands::orchestrator::origin_guard::OriginAllowList;
use crate::tests::common::harn_state_lock::lock_harn_state;

fn manifest_binding_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
    TriggerBindingSpec {
        id: id.to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "a2a-push".to_string(),
        provider: ProviderId::from("a2a-push"),
        autonomy_tier: harn_vm::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Worker {
            queue: "triage".to_string(),
        },
        dispatch_priority: harn_vm::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry: TriggerRetryConfig::default(),
        match_events: vec!["a2a.task.received".to_string()],
        dedupe_key: None,
        dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: harn_vm::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("listener-test".to_string()),
        definition_fingerprint: fingerprint.to_string(),
    }
}

fn route(path: &str, version: u32) -> RouteConfig {
    RouteConfig {
        trigger_id: "incoming-review-task".to_string(),
        binding_version: version,
        provider: ProviderId::from("a2a-push"),
        path: path.to_string(),
        auth_mode: AuthMode::Public,
        signature_mode: SignatureMode::Unsigned,
        signing_secret: None,
        dedupe_key_template: None,
        dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
        connector_ingress: false,
        connector: None,
    }
}

#[derive(Clone)]
struct StaticSecretProvider {
    secret_id: SecretId,
    secret: String,
}

#[async_trait::async_trait]
impl SecretProvider for StaticSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        if id == &self.secret_id {
            Ok(SecretBytes::from(self.secret.clone()))
        } else {
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Ok(())
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        Ok(RotationHandle {
            provider: self.namespace().to_string(),
            id: id.clone(),
            from_version: None,
            to_version: None,
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        "listener-test"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

fn webhook_route(path: &str) -> RouteConfig {
    RouteConfig {
        trigger_id: "github-webhook".to_string(),
        binding_version: 1,
        provider: ProviderId::from("github"),
        path: path.to_string(),
        auth_mode: AuthMode::Public,
        signature_mode: SignatureMode::GitHub,
        signing_secret: Some(SecretId::new("github", "test-signing-secret")),
        dedupe_key_template: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
        connector_ingress: false,
        connector: None,
    }
}

fn github_signature(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    let encoded = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256={encoded}")
}

fn authorized_acp_request(
    addr: std::net::SocketAddr,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request = format!("ws://{addr}{ACP_PATH}")
        .into_client_request()
        .expect("client request");
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        "Bearer ws-test-key".parse().expect("authorization header"),
    );
    request
}

async fn next_acp_text(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> JsonValue {
    loop {
        let message = socket
            .next()
            .await
            .expect("websocket message")
            .expect("websocket ok");
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            return serde_json::from_str(&text).expect("json-rpc text");
        }
    }
}

async fn acp_request(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
    params: JsonValue,
) -> JsonValue {
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send acp request");
    loop {
        let message = next_acp_text(socket).await;
        if message.get("method").is_some() && message.get("id").is_some() {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({
                        "jsonrpc": "2.0",
                        "id": message["id"].clone(),
                        "result": {},
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send host response");
            continue;
        }
        if message.get("id").and_then(JsonValue::as_u64) == Some(id) {
            return message;
        }
    }
}

async fn new_acp_session(addr: std::net::SocketAddr) -> String {
    let (mut socket, _) = tokio_tungstenite::connect_async(authorized_acp_request(addr))
        .await
        .expect("connect acp websocket");
    let response = acp_request(&mut socket, 1, "session/new", json!({})).await;
    response["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string()
}

async fn send_acp_request(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
    params: JsonValue,
) {
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send acp request");
}

async fn send_acp_response(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    result: JsonValue,
) {
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send acp response");
}

async fn start_acp_test_listener() -> (ListenerRuntime, Arc<AnyEventLog>, TempDir) {
    start_acp_test_listener_with_env(ListenerRuntimeEnv::for_test()).await
}

async fn start_acp_test_listener_with_env(
    runtime_env: ListenerRuntimeEnv,
) -> (ListenerRuntime, Arc<AnyEventLog>, TempDir) {
    let dir = tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let listener = ListenerRuntime::start_with_env(
        ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(harn_vm::secrets::EnvSecretProvider::new(
                "harn/listener-test",
            )),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: Vec::new(),
            tenant_store: None,
            session_store: None,
            public_metrics: false,
        },
        runtime_env,
    )
    .await
    .expect("start listener");
    (listener, log, dir)
}

async fn wait_for_acp_session_detached(listener: &ListenerRuntime, session_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !listener.acp_session_is_detached_for_test(session_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ACP session detached");
}

async fn pending_events(log: &Arc<AnyEventLog>) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
    log.read_range(&Topic::new(PENDING_TOPIC).expect("pending topic"), None, 16)
        .await
        .expect("read pending events")
}

async fn claim_events(log: &Arc<AnyEventLog>) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
    log.read_range(
        &Topic::new(harn_vm::TRIGGER_INBOX_CLAIMS_TOPIC).expect("claims topic"),
        None,
        16,
    )
    .await
    .expect("read claim events")
}

#[tokio::test(flavor = "current_thread")]
async fn readyz_tracks_listener_readiness_gate() {
    let _guard = lock_harn_state();
    reset_active_event_log();
    let (listener, _log, _dir) = start_acp_test_listener().await;
    let url = format!("{}/readyz", listener.url());
    let client = reqwest::Client::new();

    let response = client.get(&url).send().await.expect("readyz before ready");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    listener.mark_ready();
    let response = client.get(&url).send().await.expect("readyz after ready");
    assert_eq!(response.status(), StatusCode::OK);

    listener.mark_not_ready();
    let response = client
        .get(&url)
        .send()
        .await
        .expect("readyz after not ready");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
}

#[tokio::test(flavor = "current_thread")]
async fn listener_auth_accepts_durable_session_bearer() {
    let _guard = lock_harn_state();
    let session_id = "harn_sess_listener_abcdefghijklmnopqrstuvwxyz0123456789";
    let log = Arc::new(AnyEventLog::Memory(
        harn_vm::event_log::MemoryEventLog::new(32),
    ));
    let session_store = Arc::new(harn_vm::SessionStore::new(log.clone()));
    let created_at = time::OffsetDateTime::from_unix_timestamp(0).expect("unix epoch");
    let expires_at = time::OffsetDateTime::parse(
        "9999-01-01T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .expect("far future expiry");
    session_store
        .create(harn_vm::CreateSession {
            id: Some(session_id.to_string()),
            principal: "user-1".to_string(),
            created_at: Some(created_at),
            expires_at,
            attributes: BTreeMap::new(),
        })
        .await
        .expect("create durable session");

    let auth = ListenerAuth::from_config(
        true,
        Some(session_store.clone()),
        ListenerAuthConfig::default(),
    )
    .expect("session auth config");
    assert!(auth.has_credentials());
    let mut headers = BTreeMap::new();
    headers.insert("authorization".to_string(), format!("Bearer {session_id}"));

    auth.authorize(log.as_ref(), "POST", "/hooks/a2a", &headers, &[])
        .await
        .expect("session bearer authorizes");
    let touched = session_store
        .get(session_id, created_at)
        .await
        .expect("get touched session")
        .expect("session remains active");
    assert!(touched.last_seen_at > created_at);
}

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_requires_configured_bearer_auth() {
    let _guard = lock_harn_state();
    reset_active_event_log();

    let (listener, _log, _dir) = start_acp_test_listener_with_env(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
    )
    .await;

    let unauthorized =
        tokio_tungstenite::connect_async(format!("ws://{}{}", listener.local_addr(), ACP_PATH))
            .await;
    assert!(unauthorized.is_err(), "missing bearer should fail upgrade");

    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("authorized connect");
    let response = acp_request(&mut socket, 1, "initialize", json!({})).await;
    assert_eq!(response["result"]["agentInfo"]["name"], "harn");
    assert_eq!(response["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        response["result"]["agentCapabilities"]["mcpCapabilities"],
        json!({
            "http": true,
            "sse": true,
        })
    );
    assert_eq!(
        response["result"]["agentCapabilities"]["sessionCapabilities"],
        json!({
            "close": {},
            "list": {},
            "resume": {},
            "restoreToolCall": {},
            "cancelToolCall": {},
        })
    );
    assert!(
        response["result"]["agentCapabilities"]["sessionCapabilities"]
            .get("fork")
            .is_none()
    );
    assert_eq!(response["result"]["authMethods"], json!([]));

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_parallel_clients_get_distinct_sessions_and_can_load_active_session() {
    let _guard = lock_harn_state();
    reset_active_event_log();

    let (listener, _log, _dir) = start_acp_test_listener_with_env(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
    )
    .await;

    let (first, second) = tokio::join!(
        new_acp_session(listener.local_addr()),
        new_acp_session(listener.local_addr())
    );
    assert_ne!(first, second);

    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("authorized connect");
    let created = acp_request(&mut socket, 1, "session/new", json!({})).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    let loaded = acp_request(
        &mut socket,
        2,
        "session/load",
        json!({"sessionId": session_id}),
    )
    .await;
    assert_eq!(
        loaded["result"]["session"]["sessionId"],
        created["result"]["sessionId"]
    );
    let prompted = acp_request(
        &mut socket,
        3,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "__io_println(\"websocket prompt\")"}],
        }),
    )
    .await;
    assert_eq!(prompted["result"]["stopReason"], "end_turn");

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_rejects_duplicate_attach_to_live_session() {
    let _guard = lock_harn_state();
    reset_active_event_log();

    let (listener, _log, _dir) = start_acp_test_listener_with_env(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
    )
    .await;
    let (mut first_socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("first connect");
    let created = acp_request(&mut first_socket, 1, "session/new", json!({})).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    let (mut second_socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("second connect");
    let loaded = acp_request(
        &mut second_socket,
        2,
        "session/load",
        json!({"sessionId": session_id}),
    )
    .await;
    assert_eq!(loaded["error"]["code"], json!(-32010));

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_reconnect_replays_pending_host_request_and_completes_prompt() {
    let _guard = lock_harn_state();
    reset_active_event_log();

    let (listener, _log, _dir) = start_acp_test_listener_with_env(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
    )
    .await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("connect");
    let created = acp_request(&mut socket, 1, "session/new", json!({})).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    send_acp_request(
        &mut socket,
        2,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "__io_println(\"reconnect\")"}],
        }),
    )
    .await;
    let host_request = loop {
        let message = next_acp_text(&mut socket).await;
        if message.get("method").is_some() && message.get("id").is_some() {
            break message;
        }
    };
    let host_request_id = host_request["id"].as_u64().expect("host request id");
    let replay_from = host_request["_harn"]["eventId"]
        .as_u64()
        .expect("host request event id")
        .saturating_sub(1);
    socket.close(None).await.expect("close first socket");
    drop(socket);
    tokio::task::yield_now().await;

    let (mut reconnected, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("reconnect");
    send_acp_request(
        &mut reconnected,
        3,
        "session/load",
        json!({
            "sessionId": session_id,
            "lastAckedEventId": replay_from,
        }),
    )
    .await;

    let mut saw_replayed_host_request = false;
    let mut saw_load_response = false;
    let mut saw_prompt_response = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !(saw_replayed_host_request && saw_load_response && saw_prompt_response) {
            let message = next_acp_text(&mut reconnected).await;
            if message.get("method").is_some() && message.get("id").is_some() {
                if message.get("id").and_then(JsonValue::as_u64) == Some(host_request_id) {
                    assert_eq!(message["_harn"]["replayed"], json!(true));
                    saw_replayed_host_request = true;
                }
                let id = message["id"].as_u64().expect("host request id");
                send_acp_response(&mut reconnected, id, json!({})).await;
            } else if message.get("id").and_then(JsonValue::as_u64) == Some(3) {
                assert_eq!(message["result"]["session"]["sessionId"], json!(session_id));
                saw_load_response = true;
            } else if message.get("id").and_then(JsonValue::as_u64) == Some(2) {
                assert_eq!(message["result"]["stopReason"], json!("end_turn"));
                saw_prompt_response = true;
            }
        }
    })
    .await
    .expect("reconnect flow completed");

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_replays_serialized_events_after_worker_expiry() {
    let _guard = lock_harn_state();
    reset_active_event_log();

    let (listener, _log, _dir) = start_acp_test_listener_with_env(
        ListenerRuntimeEnv::for_test()
            .with_api_key("ws-test-key")
            .with_acp_retained_session_duration(Duration::ZERO),
    )
    .await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("connect");
    let created = acp_request(&mut socket, 1, "session/new", json!({})).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    let replay_from = created["_harn"]["eventId"]
        .as_u64()
        .expect("created event id")
        .saturating_sub(1);
    socket.close(None).await.expect("close socket");
    drop(socket);
    wait_for_acp_session_detached(&listener, &session_id).await;
    listener.sweep_expired_acp_workers_for_test().await;

    let (mut reconnected, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("reconnect");
    send_acp_request(
        &mut reconnected,
        4,
        "session/load",
        json!({
            "sessionId": session_id,
            "lastAckedEventId": replay_from,
        }),
    )
    .await;

    let mut saw_persisted_replay = false;
    let mut saw_expired_session_error = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !(saw_persisted_replay && saw_expired_session_error) {
            let message = next_acp_text(&mut reconnected).await;
            if message["_harn"]["replayed"] == json!(true) {
                assert_eq!(message["result"]["sessionId"], json!(session_id));
                saw_persisted_replay = true;
            }
            if message.get("id").and_then(JsonValue::as_u64) == Some(4) {
                assert_eq!(message["error"]["code"], json!(-32004));
                saw_expired_session_error = true;
            }
        }
    })
    .await
    .expect("expired replay flow completed");

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn reload_swaps_routes_without_losing_inflight_request() {
    let _guard = lock_harn_state();
    reset_active_event_log();
    harn_vm::clear_trigger_registry();

    let dir = tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    harn_vm::install_manifest_triggers(vec![manifest_binding_spec("incoming-review-task", "v1")])
        .await
        .expect("install v1 binding");

    let request_entered_path = dir.path().join("request-entered");
    let request_release_path = dir.path().join("request-release");
    let listener = ListenerRuntime::start_with_env(
        ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(harn_vm::secrets::EnvSecretProvider::new(
                "harn/listener-test",
            )),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![route("/a2a/v1", 1)],
            tenant_store: None,
            session_store: None,
            public_metrics: false,
        },
        ListenerRuntimeEnv::for_test().with_request_gate(TestRequestGate {
            entered_file: Some(request_entered_path.clone()),
            release_file: Some(request_release_path.clone()),
        }),
    )
    .await
    .expect("start listener");

    let client = reqwest::Client::new();
    let first_url = format!("http://{}/a2a/v1", listener.local_addr());
    let second_url = format!("http://{}/a2a/v2", listener.local_addr());

    let first_request = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .post(first_url)
                .json(&json!({"task_id": "task-1", "sender": "alpha"}))
                .send()
                .await
                .expect("first request")
                .status()
        })
    };

    wait_for_test_release_file(&request_entered_path).await;
    harn_vm::install_manifest_triggers(vec![manifest_binding_spec("incoming-review-task", "v2")])
        .await
        .expect("install v2 binding");
    listener
        .reload_routes(vec![route("/a2a/v2", 2)])
        .expect("reload listener routes");
    tokio::fs::write(&request_release_path, b"release")
        .await
        .expect("release first request");

    assert_eq!(
        first_request.await.expect("join first request"),
        StatusCode::OK
    );
    assert_eq!(
        client
            .post(&second_url)
            .json(&json!({"task_id": "task-2", "sender": "beta"}))
            .send()
            .await
            .expect("second request")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .post(format!("http://{}/a2a/v1", listener.local_addr()))
            .json(&json!({"task_id": "task-old", "sender": "gamma"}))
            .send()
            .await
            .expect("old route request")
            .status(),
        StatusCode::NOT_FOUND
    );

    let pending_topic = Topic::new(PENDING_TOPIC).expect("pending topic");
    let events = log
        .read_range(&pending_topic, None, 16)
        .await
        .expect("read pending events");
    let versions: Vec<u64> = events
        .iter()
        .filter_map(|(_, event)| {
            event
                .payload
                .get("binding_version")
                .and_then(JsonValue::as_u64)
        })
        .collect();
    let task_ids: Vec<String> = events
        .iter()
        .filter_map(|(_, event)| {
            event
                .payload
                .get("event")
                .and_then(|value| value.get("provider_payload"))
                .and_then(|value| value.get("task_id"))
                .and_then(JsonValue::as_str)
                .map(|value| value.to_string())
        })
        .collect();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(task_ids, vec!["task-1".to_string(), "task-2".to_string()]);

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
    harn_vm::clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_first_delivery_is_appended() {
    let _guard = lock_harn_state();
    reset_active_event_log();
    let dir = tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let listener = ListenerRuntime::start_with_env(
        ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![webhook_route("/hooks/github")],
            tenant_store: None,
            session_store: None,
            public_metrics: false,
        },
        ListenerRuntimeEnv::for_test(),
    )
    .await
    .expect("start listener");

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let response = reqwest::Client::new()
        .post(format!("http://{}/hooks/github", listener.local_addr()))
        .header("X-GitHub-Event", "issues")
        .header("X-GitHub-Delivery", "delivery-1")
        .header("X-Hub-Signature-256", github_signature("topsecret", body))
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("send webhook");

    assert_eq!(response.status(), StatusCode::OK);
    let payload: JsonValue = response.json().await.expect("response json");
    assert_eq!(
        payload.get("status"),
        Some(&JsonValue::String("accepted".to_string()))
    );

    let events = pending_events(&log).await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]
            .1
            .payload
            .get("event")
            .and_then(|value| value.get("dedupe_key"))
            .and_then(JsonValue::as_str),
        Some("delivery-1")
    );

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_ingest_saturation_returns_retry_after() {
    let _guard = lock_harn_state();
    reset_active_event_log();

    let dir = tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let listener = ListenerRuntime::start_with_env(
        ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: metrics.clone(),
            admin_reload: None,
            mcp_router: None,
            routes: vec![webhook_route("/hooks/github")],
            tenant_store: None,
            session_store: None,
            public_metrics: false,
        },
        ListenerRuntimeEnv::for_test().with_ingest_backpressure(IngestBackpressureConfig {
            global_capacity: 100,
            per_source_capacity: 1,
            refill_per_sec: 0,
        }),
    )
    .await
    .expect("start listener");

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let signature = github_signature("topsecret", body);
    let client = reqwest::Client::new();
    let url = format!("http://{}/hooks/github", listener.local_addr());

    let first = client
        .post(&url)
        .header("X-GitHub-Event", "issues")
        .header("X-GitHub-Delivery", "delivery-1")
        .header("X-Hub-Signature-256", &signature)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("send first webhook");
    assert_eq!(first.status(), StatusCode::OK);

    let saturated = client
        .post(&url)
        .header("X-GitHub-Event", "issues")
        .header("X-GitHub-Delivery", "delivery-2")
        .header("X-Hub-Signature-256", &signature)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("send saturated webhook");
    assert_eq!(saturated.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        saturated
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );

    let events = pending_events(&log).await;
    assert_eq!(events.len(), 1);
    assert!(metrics
        .render_prometheus()
        .contains("harn_backpressure_events_total{action=\"reject\",dimension=\"ingest\"} 1"));

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_duplicate_delivery_is_dropped() {
    let _guard = lock_harn_state();
    reset_active_event_log();
    let dir = tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let listener = ListenerRuntime::start_with_env(
        ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![webhook_route("/hooks/github")],
            tenant_store: None,
            session_store: None,
            public_metrics: false,
        },
        ListenerRuntimeEnv::for_test(),
    )
    .await
    .expect("start listener");

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let signature = github_signature("topsecret", body);
    let client = reqwest::Client::new();
    let url = format!("http://{}/hooks/github", listener.local_addr());

    let first = client
        .post(&url)
        .header("X-GitHub-Event", "issues")
        .header("X-GitHub-Delivery", "delivery-1")
        .header("X-Hub-Signature-256", &signature)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("send first webhook");
    assert_eq!(first.status(), StatusCode::OK);

    let duplicate = client
        .post(&url)
        .header("X-GitHub-Event", "issues")
        .header("X-GitHub-Delivery", "delivery-1")
        .header("X-Hub-Signature-256", &signature)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("send duplicate webhook");

    assert_eq!(duplicate.status(), StatusCode::OK);
    let payload: JsonValue = duplicate.json().await.expect("duplicate response json");
    assert_eq!(
        payload.get("status"),
        Some(&JsonValue::String("duplicate_dropped".to_string()))
    );

    let events = pending_events(&log).await;
    assert_eq!(events.len(), 1);

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_dedupe_claim_uses_route_retention_days() {
    let _guard = lock_harn_state();
    reset_active_event_log();
    let dir = tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let mut route = webhook_route("/hooks/github");
    route.dedupe_retention_days = 3;
    let listener = ListenerRuntime::start_with_env(
        ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![route],
            tenant_store: None,
            session_store: None,
            public_metrics: false,
        },
        ListenerRuntimeEnv::for_test(),
    )
    .await
    .expect("start listener");

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let response = reqwest::Client::new()
        .post(format!("http://{}/hooks/github", listener.local_addr()))
        .header("X-GitHub-Event", "issues")
        .header("X-GitHub-Delivery", "delivery-ttl")
        .header("X-Hub-Signature-256", github_signature("topsecret", body))
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("send webhook");

    assert_eq!(response.status(), StatusCode::OK);
    let claims = claim_events(&log).await;
    assert_eq!(claims.len(), 1);
    let claim_event = &claims[0].1;
    let claim = &claim_event.payload;
    assert_eq!(
        claim.get("binding_id").and_then(JsonValue::as_str),
        Some("github-webhook")
    );
    assert_eq!(
        claim.get("dedupe_key").and_then(JsonValue::as_str),
        Some("delivery-ttl")
    );
    let expires_at_ms = claim
        .get("expires_at_ms")
        .and_then(JsonValue::as_i64)
        .expect("claim expires_at_ms");
    let ttl_ms = 3 * 24 * 60 * 60 * 1000;
    let expected_upper = claim_event.occurred_at_ms + ttl_ms;
    assert!(
        (expected_upper - 1000..=expected_upper).contains(&expires_at_ms),
        "expires_at_ms {expires_at_ms} should use 3 day route retention"
    );

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}
