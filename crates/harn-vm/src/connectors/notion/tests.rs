use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::connectors::{
    Connector, ConnectorCtx, InboxIndex, MetricsRegistry, RateLimiterFactory, RawInbound,
    TriggerBinding,
};
use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::secrets::{SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider};
use crate::triggers::{ProviderId, SignatureStatus, TRIGGER_INBOX_ENVELOPES_TOPIC};

const VERIFICATION_TOKEN: &str = "secret_token_for_testing";
const API_TOKEN: &str = "secret-api-token";

struct StaticSecretProvider {
    namespace: String,
    secrets: HashMap<SecretId, String>,
}

impl StaticSecretProvider {
    fn new(namespace: &str, secrets: HashMap<SecretId, String>) -> Self {
        Self {
            namespace: namespace.to_string(),
            secrets,
        }
    }
}

#[async_trait]
impl SecretProvider for StaticSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        self.secrets
            .get(id)
            .cloned()
            .map(SecretBytes::from)
            .ok_or_else(|| SecretError::NotFound {
                provider: self.namespace.clone(),
                id: id.clone(),
            })
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "put",
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<crate::secrets::RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "rotate",
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

async fn test_ctx(secrets: Arc<dyn SecretProvider>) -> ConnectorCtx {
    let event_log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(128)));
    let metrics = Arc::new(MetricsRegistry::default());
    let inbox = Arc::new(
        InboxIndex::new(event_log.clone(), metrics.clone())
            .await
            .expect("inbox init"),
    );
    ConnectorCtx {
        event_log,
        secrets,
        inbox,
        metrics,
        rate_limiter: Arc::new(RateLimiterFactory::default()),
    }
}

async fn read_topic(log: &Arc<AnyEventLog>, topic: &str) -> Vec<(u64, crate::event_log::LogEvent)> {
    log.read_range(
        &crate::event_log::Topic::new(topic).unwrap(),
        None,
        usize::MAX,
    )
    .await
    .unwrap()
}

fn webhook_binding(with_secret: bool) -> TriggerBinding {
    let mut binding = TriggerBinding::new(ProviderId::from("notion"), "webhook", "notion.webhook");
    binding.config = json!({
        "match": { "path": "/hooks/notion" },
        "secrets": {
            "verification_token": if with_secret {
                JsonValue::String("notion/verification-token".to_string())
            } else {
                JsonValue::Null
            }
        },
        "webhook": {},
    });
    binding
}

fn poll_binding() -> TriggerBinding {
    let mut binding = TriggerBinding::new(ProviderId::from("notion"), "poll", "notion.poll");
    binding.config = json!({
        "secrets": {
            "api_token": "notion/api-token"
        },
        "poll": {
            "resource": "data_source",
            "data_source_id": "ds_123",
            "interval_secs": 60,
            "high_water_mark": "last_edited_time",
            "page_size": 100,
        }
    });
    binding
}

fn with_base(args: JsonValue, base_url: &str) -> JsonValue {
    let mut map = args.as_object().cloned().unwrap_or_default();
    map.insert(
        "api_base_url".to_string(),
        JsonValue::String(base_url.to_string()),
    );
    map.insert(
        "api_token".to_string(),
        JsonValue::String(API_TOKEN.to_string()),
    );
    map.insert(
        "notion_version".to_string(),
        JsonValue::String(DEFAULT_NOTION_API_VERSION.to_string()),
    );
    JsonValue::Object(map)
}

fn hmac_sha256(secret: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK_SIZE: usize = 64;
    let mut key = if secret.len() > BLOCK_SIZE {
        Sha256::digest(secret).to_vec()
    } else {
        secret.to_vec()
    };
    key.resize(BLOCK_SIZE, 0);
    let mut inner_pad = vec![0x36; BLOCK_SIZE];
    let mut outer_pad = vec![0x5c; BLOCK_SIZE];
    for (slot, key_byte) in inner_pad.iter_mut().zip(&key) {
        *slot ^= key_byte;
    }
    for (slot, key_byte) in outer_pad.iter_mut().zip(&key) {
        *slot ^= key_byte;
    }
    let mut inner = Sha256::new();
    inner.update(&inner_pad);
    inner.update(data);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&outer_pad);
    outer.update(inner_digest);
    outer.finalize().to_vec()
}

fn notion_signature(secret: &str, body: &[u8]) -> String {
    format!(
        "sha256={}",
        hex::encode(hmac_sha256(secret.as_bytes(), body))
    )
}

fn signed_raw_inbound(body: &JsonValue) -> RawInbound {
    let encoded = serde_json::to_vec(body).unwrap();
    let mut raw = RawInbound::new(
        "",
        BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "X-Notion-Signature".to_string(),
                notion_signature(VERIFICATION_TOKEN, &encoded),
            ),
            ("request-id".to_string(), "req_123".to_string()),
        ]),
        encoded,
    );
    raw.received_at = OffsetDateTime::parse(
        "2026-04-19T12:34:56Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    raw.metadata = json!({ "binding_id": "notion.webhook" });
    raw
}

#[tokio::test]
async fn notion_webhook_handshake_is_captured_for_doctor() {
    let secrets = Arc::new(StaticSecretProvider::new("notion", HashMap::new()));
    let ctx = test_ctx(secrets).await;
    let log = ctx.event_log.clone();
    let mut connector = NotionConnector::new();
    connector.init(ctx).await.unwrap();
    connector.activate(&[webhook_binding(false)]).await.unwrap();

    let mut raw = RawInbound::new(
        "",
        BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]),
        br#"{"verification_token":"secret_verification"}"#.to_vec(),
    );
    raw.received_at = OffsetDateTime::parse(
        "2026-04-19T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    raw.metadata = json!({ "binding_id": "notion.webhook" });

    let event = connector.normalize_inbound(raw).await.unwrap();
    assert_eq!(event.kind, "subscription.verification");
    assert_eq!(event.signature_status, SignatureStatus::Unsigned);

    let handshakes = load_pending_webhook_handshakes(log.as_ref()).await.unwrap();
    let handshake = handshakes.get("notion.webhook").unwrap();
    assert_eq!(handshake.verification_token, "secret_verification");
    assert_eq!(handshake.path.as_deref(), Some("/hooks/notion"));
}

#[tokio::test]
async fn notion_webhook_signed_event_normalizes_to_typed_payload() {
    let secrets = Arc::new(StaticSecretProvider::new(
        "notion",
        HashMap::from([(
            SecretId::new("notion", "verification-token"),
            VERIFICATION_TOKEN.to_string(),
        )]),
    ));
    let mut connector = NotionConnector::new();
    connector.init(test_ctx(secrets).await).await.unwrap();
    connector.activate(&[webhook_binding(true)]).await.unwrap();

    let body = json!({
        "id": "evt_1",
        "timestamp": "2026-04-19T12:34:56Z",
        "type": "page.content_updated",
        "workspace_id": "ws_1",
        "subscription_id": "sub_1",
        "integration_id": "int_1",
        "entity": {
            "id": "page_1",
            "type": "page"
        }
    });
    let event = connector
        .normalize_inbound(signed_raw_inbound(&body))
        .await
        .unwrap();
    assert_eq!(event.kind, "page.content_updated");
    assert_eq!(event.signature_status, SignatureStatus::Verified);
    assert!(event.dedupe_key.starts_with("notion:page_1:"));
    let ProviderPayload::Known(KnownProviderPayload::Notion(payload)) = event.provider_payload
    else {
        panic!("expected notion payload");
    };
    assert_eq!(payload.request_id.as_deref(), Some("req_123"));
    assert_eq!(payload.entity_id.as_deref(), Some("page_1"));
    assert_eq!(payload.entity_type.as_deref(), Some("page"));
    assert_eq!(payload.subscription_id.as_deref(), Some("sub_1"));
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

fn accept_with_deadline(listener: &TcpListener, label: &str) -> std::net::TcpStream {
    listener.set_nonblocking(true).expect("set nonblocking");
    let deadline = std::time::Instant::now() + StdDuration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    panic!("{label}: no client within 3s");
                }
                std::thread::sleep(StdDuration::from_millis(10));
            }
            Err(error) => panic!("{label}: accept failed: {error}"),
        }
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut temp = [0u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut temp).expect("read request");
        assert!(n > 0, "request ended before headers");
        buffer.extend_from_slice(&temp[..n]);
        if let Some(idx) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = idx + 4;
            break;
        }
    }
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines.next().unwrap();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or_default().to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let n = stream.read(&mut temp).expect("read body");
        assert!(n > 0, "request ended before body");
        buffer.extend_from_slice(&temp[..n]);
    }
    let body =
        String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).to_string();
    CapturedRequest {
        method,
        path,
        headers,
        body,
    }
}

fn write_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        429 => "Too Many Requests",
        _ => "OK",
    };
    let retry_after = if status == 429 {
        "retry-after: 0\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\n{}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        status,
        status_text,
        retry_after,
        body.len(),
        body,
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn spawn_mock_server(
    expected_requests: usize,
    responder: impl Fn(usize, &CapturedRequest) -> (u16, String) + Send + 'static,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        for index in 0..expected_requests {
            let mut stream = accept_with_deadline(&listener, "notion mock server");
            let request = read_request(&mut stream);
            captured.lock().unwrap().push(request.clone());
            let (status, body) = responder(index, &request);
            write_response(&mut stream, status, &body);
        }
    });
    (format!("http://{}", addr), handle)
}

#[tokio::test]
async fn notion_client_methods_use_current_api_headers_and_paths() {
    let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let (base_url, handle) = spawn_mock_server(
        7,
        |_index, request| {
            let body = if request.path.starts_with("/data_sources/") {
                json!({"results":[],"has_more":false}).to_string()
            } else {
                json!({"ok":true}).to_string()
            };
            (200, body)
        },
        captured.clone(),
    );

    let secrets = Arc::new(StaticSecretProvider::new("notion", HashMap::new()));
    let mut connector = NotionConnector::new();
    connector.init(test_ctx(secrets).await).await.unwrap();
    let client = connector.client();

    client
        .call("get_page", with_base(json!({"id":"page_1"}), &base_url))
        .await
        .unwrap();
    client
        .call(
            "update_page",
            with_base(
                json!({"id":"page_1","properties":{"Name":{"title":[]}}}),
                &base_url,
            ),
        )
        .await
        .unwrap();
    client
        .call(
            "append_blocks",
            with_base(
                json!({"page_id":"page_1","blocks":[{"object":"block"}]}),
                &base_url,
            ),
        )
        .await
        .unwrap();
    client
        .call(
            "query_database",
            with_base(
                json!({"id":"ds_1","filter":{"timestamp":"last_edited_time"}}),
                &base_url,
            ),
        )
        .await
        .unwrap();
    client
        .call("search", with_base(json!({"query":"bugs"}), &base_url))
        .await
        .unwrap();
    client
        .call(
            "create_comment",
            with_base(
                json!({"page_id":"page_1","rich_text":[{"type":"text","text":{"content":"hi"}}]}),
                &base_url,
            ),
        )
        .await
        .unwrap();
    client
        .call(
            "api_call",
            with_base(json!({"path":"/pages/page_1","method":"GET"}), &base_url),
        )
        .await
        .unwrap();

    handle.join().unwrap();
    let requests = captured.lock().unwrap().clone();
    assert_eq!(requests.len(), 7);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/pages/page_1");
    assert_eq!(
        requests[0]
            .headers
            .get("notion-version")
            .map(String::as_str),
        Some(DEFAULT_NOTION_API_VERSION)
    );
    assert_eq!(requests[1].method, "PATCH");
    assert_eq!(requests[1].path, "/pages/page_1");
    assert!(requests[1].body.contains("\"properties\""));
    assert_eq!(requests[2].path, "/blocks/page_1/children");
    assert!(requests[2].body.contains("\"children\""));
    assert_eq!(requests[3].path, "/data_sources/ds_1/query");
    assert_eq!(requests[4].path, "/search");
    assert_eq!(requests[5].path, "/comments");
    assert!(requests[5].body.contains("\"page_id\":\"page_1\""));
    assert_eq!(requests[6].path, "/pages/page_1");
}

#[tokio::test]
async fn notion_client_retries_once_after_rate_limit() {
    let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let (base_url, handle) = spawn_mock_server(
        2,
        |index, _request| {
            if index == 0 {
                (
                    429,
                    json!({"object":"error","code":"rate_limited"}).to_string(),
                )
            } else {
                (200, json!({"object":"page","id":"page_1"}).to_string())
            }
        },
        captured.clone(),
    );

    let secrets = Arc::new(StaticSecretProvider::new("notion", HashMap::new()));
    let ctx = test_ctx(secrets).await;
    let log = ctx.event_log.clone();
    let mut connector = NotionConnector::new();
    connector.init(ctx).await.unwrap();
    let client = connector.client();

    let result = client
        .call("get_page", with_base(json!({"id":"page_1"}), &base_url))
        .await
        .unwrap();

    handle.join().unwrap();
    let requests = captured.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(result.get("id").and_then(JsonValue::as_str), Some("page_1"));

    let observations = read_topic(&log, NOTION_RATE_LIMIT_TOPIC).await;
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0]
            .1
            .payload
            .get("status")
            .and_then(JsonValue::as_u64),
        Some(429)
    );
}

#[tokio::test]
async fn notion_poll_binding_emits_targeted_inbox_event_and_persists_high_water() {
    let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let page = json!({
        "id": "page_1",
        "last_edited_time": "2026-04-19T10:00:00Z",
        "properties": {
            "Name": {
                "type": "title",
                "title": [{"plain_text": "Task"}]
            }
        }
    });
    let (base_url, handle) = spawn_mock_server(
        1,
        move |_index, _request| {
            (
                200,
                json!({
                    "results": [page],
                    "has_more": false,
                    "next_cursor": JsonValue::Null,
                })
                .to_string(),
            )
        },
        captured,
    );

    let secrets = Arc::new(StaticSecretProvider::new(
        "notion",
        HashMap::from([(SecretId::new("notion", "api-token"), API_TOKEN.to_string())]),
    ));
    let ctx = test_ctx(secrets).await;
    let log = ctx.event_log.clone();
    let mut connector = NotionConnector::new();
    connector.init(ctx).await.unwrap();

    let mut binding = poll_binding();
    binding.config = json!({
        "secrets": { "api_token": "notion/api-token" },
        "poll": {
            "resource": "data_source",
            "data_source_id": "ds_123",
            "interval_secs": 60,
            "page_size": 100,
            "high_water_mark": "last_edited_time"
        }
    });

    std::env::set_var("HARN_TEST_NOTION_API_BASE_URL", &base_url);
    connector.activate(&[binding]).await.unwrap();

    for _ in 0..40 {
        if !read_topic(&log, TRIGGER_INBOX_ENVELOPES_TOPIC)
            .await
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }

    let inbox = read_topic(&log, TRIGGER_INBOX_ENVELOPES_TOPIC).await;
    assert_eq!(inbox.len(), 1);
    let payload = &inbox[0].1.payload;
    assert_eq!(
        payload.get("trigger_id").and_then(JsonValue::as_str),
        Some("notion.poll")
    );
    assert_eq!(
        payload
            .get("event")
            .and_then(|value| value.get("kind"))
            .and_then(JsonValue::as_str),
        Some("page.content_updated")
    );
    let state_events = read_topic(&log, NOTION_POLL_STATE_TOPIC).await;
    assert_eq!(state_events.len(), 1);
    let cache_events = read_topic(&log, NOTION_POLL_CACHE_TOPIC).await;
    assert_eq!(cache_events.len(), 1);

    connector.shutdown(StdDuration::from_secs(1)).await.unwrap();
    std::env::remove_var("HARN_TEST_NOTION_API_BASE_URL");
    handle.join().unwrap();
}
