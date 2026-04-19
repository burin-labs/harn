use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::connectors::{
    Connector, ConnectorClient, ConnectorCtx, InboxIndex, MetricsRegistry, RateLimiterFactory,
    RawInbound, TriggerBinding,
};
use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::secrets::{SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider};
use crate::triggers::event::KnownProviderPayload;
use crate::triggers::{ProviderId, SlackEventPayload};

const SIGNING_SECRET: &str = "8f742231b10e8888abcd99yyyzzz85a5";
const BOT_TOKEN: &str = "xoxb-test-token";

struct StaticSecretProvider {
    namespace: String,
    secrets: BTreeMap<SecretId, String>,
}

impl StaticSecretProvider {
    fn new(namespace: &str, secrets: BTreeMap<SecretId, String>) -> Self {
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
    let event_log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)));
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

async fn connector() -> SlackConnector {
    let secrets = Arc::new(StaticSecretProvider::new(
        "slack",
        BTreeMap::from([
            (
                SecretId::new("slack", "test-signing-secret"),
                SIGNING_SECRET.to_string(),
            ),
            (SecretId::new("slack", "bot-token"), BOT_TOKEN.to_string()),
        ]),
    ));
    let mut connector = SlackConnector::new();
    connector.init(test_ctx(secrets).await).await.unwrap();
    connector.activate(&[binding()]).await.unwrap();
    connector
}

async fn initialized_client() -> Arc<dyn ConnectorClient> {
    Arc::new(connector().await).client()
}

fn binding() -> TriggerBinding {
    let mut binding = TriggerBinding::new(ProviderId::from("slack"), "webhook", "slack.test");
    binding.config = json!({
        "match": { "path": "/hooks/slack" },
        "secrets": { "signing_secret": "slack/test-signing-secret" },
    });
    binding
}

fn raw_inbound(body: &JsonValue, timestamp: i64) -> RawInbound {
    let encoded = serde_json::to_vec(body).unwrap();
    let headers = slack_headers(&encoded, timestamp);
    let mut raw = RawInbound::new("", headers, encoded);
    raw.received_at = OffsetDateTime::from_unix_timestamp(timestamp).unwrap();
    raw.metadata = json!({ "binding_id": "slack.test" });
    raw
}

fn slack_headers(body: &[u8], timestamp: i64) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "X-Slack-Request-Timestamp".to_string(),
            timestamp.to_string(),
        ),
        (
            "X-Slack-Signature".to_string(),
            slack_signature(SIGNING_SECRET, timestamp, body),
        ),
    ])
}

fn slack_signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut signed = format!("v0:{timestamp}:").into_bytes();
    signed.extend_from_slice(body);
    let digest = hmac_sha256(secret.as_bytes(), &signed);
    format!("v0={}", hex::encode(digest))
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

#[derive(Clone)]
struct FixtureCase {
    name: &'static str,
    body: JsonValue,
    expected_kind: &'static str,
}

#[tokio::test]
async fn slack_connector_normalizes_docs_fixtures_into_typed_payloads() {
    let connector = connector().await;
    let timestamp = 1_715_000_000;
    let cases = vec![
        FixtureCase {
            name: "message.channels",
            expected_kind: "message.channels",
            body: json!({
                "token": "z26uFbvR1xHJEdHE1OQiO6t8",
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "message",
                    "user": "U123ABC456",
                    "text": "hello from a channel",
                    "ts": "1715000000.000100",
                    "channel": "C123ABC456",
                    "channel_type": "channel",
                    "event_ts": "1715000000.000100"
                },
                "type": "event_callback",
                "event_id": "Ev123MESSAGE",
                "event_time": 1715000000
            }),
        },
        FixtureCase {
            name: "app_mention",
            expected_kind: "app_mention",
            body: json!({
                "token": "ZZZZZZWSxiZZZ2yIvs3peJ",
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "app_mention",
                    "user": "U123ABC456",
                    "text": "What is the hour of the pearl, <@U0LAN0Z89>?",
                    "ts": "1515449522.000016",
                    "channel": "C123ABC456",
                    "event_ts": "1515449522000016"
                },
                "type": "event_callback",
                "event_id": "Ev123ABC456",
                "event_time": 1515449522000016i64
            }),
        },
        FixtureCase {
            name: "reaction_added",
            expected_kind: "reaction_added",
            body: json!({
                "token": "z26uFbvR1xHJEdHE1OQiO6t8",
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "reaction_added",
                    "user": "U123ABC456",
                    "item": {
                        "type": "message",
                        "channel": "C123ABC456",
                        "ts": "1464196127.000002"
                    },
                    "reaction": "slightly_smiling_face",
                    "item_user": "U222222222",
                    "event_ts": "1465244570.336841"
                },
                "type": "event_callback",
                "event_id": "Ev123REACTION",
                "event_time": 1234567890
            }),
        },
        FixtureCase {
            name: "team_join",
            expected_kind: "team_join",
            body: json!({
                "token": "z26uFbvR1xHJEdHE1OQiO6t8",
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "team_join",
                    "user": {
                        "id": "U024BE7LH",
                        "name": "sam",
                        "real_name": "Sam Example"
                    },
                    "event_ts": "1360782804.083113"
                },
                "type": "event_callback",
                "event_id": "Ev123TEAMJOIN",
                "event_time": 1360782804
            }),
        },
        FixtureCase {
            name: "channel_created",
            expected_kind: "channel_created",
            body: json!({
                "token": "z26uFbvR1xHJEdHE1OQiO6t8",
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "channel_created",
                    "channel": {
                        "id": "C024BE91L",
                        "name": "fun",
                        "created": 1360782804,
                        "creator": "U024BE7LH"
                    },
                    "event_ts": "1360782804.083113"
                },
                "type": "event_callback",
                "event_id": "Ev123CHANNEL",
                "event_time": 1360782804
            }),
        },
    ];

    for case in cases {
        let event = connector
            .normalize_inbound(raw_inbound(&case.body, timestamp))
            .unwrap();
        assert_eq!(event.kind, case.expected_kind, "case {}", case.name);
        let payload = match &event.provider_payload {
            crate::triggers::ProviderPayload::Known(KnownProviderPayload::Slack(payload)) => {
                payload
            }
            other => panic!("expected slack payload, got {other:?}"),
        };
        match (case.expected_kind, payload) {
            ("message.channels", SlackEventPayload::MessageChannels(inner)) => {
                assert_eq!(inner.channel.as_deref(), Some("C123ABC456"));
            }
            ("app_mention", SlackEventPayload::AppMention(inner)) => {
                assert_eq!(inner.user.as_deref(), Some("U123ABC456"));
            }
            ("reaction_added", SlackEventPayload::ReactionAdded(inner)) => {
                assert_eq!(inner.reaction.as_deref(), Some("slightly_smiling_face"));
            }
            ("team_join", SlackEventPayload::TeamJoin(inner)) => {
                assert_eq!(inner.common.user_id.as_deref(), Some("U024BE7LH"));
            }
            ("channel_created", SlackEventPayload::ChannelCreated(inner)) => {
                assert_eq!(inner.common.channel_id.as_deref(), Some("C024BE91L"));
            }
            other => panic!("unexpected typed payload mapping: {other:?}"),
        }
        assert_eq!(
            event.signature_status,
            crate::triggers::SignatureStatus::Verified
        );
    }
}

#[tokio::test]
async fn slack_connector_accepts_url_verification_payloads() {
    let connector = connector().await;
    let payload = json!({
        "token": "legacy-token",
        "challenge": "3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P",
        "type": "url_verification"
    });
    let event = connector
        .normalize_inbound(raw_inbound(&payload, 1_715_000_000))
        .unwrap();
    assert_eq!(event.kind, "url_verification");
    match event.provider_payload {
        crate::triggers::ProviderPayload::Known(KnownProviderPayload::Slack(
            SlackEventPayload::Other(common),
        )) => {
            assert_eq!(
                common.raw.get("challenge").and_then(JsonValue::as_str),
                Some("3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P")
            );
        }
        other => panic!("expected slack other payload, got {other:?}"),
    }
}

#[tokio::test]
async fn slack_connector_rejects_tampered_signature() {
    let connector = connector().await;
    let payload = json!({
        "team_id": "T123ABC456",
        "type": "event_callback",
        "event_id": "EvBad",
        "event": {
            "type": "app_mention",
            "user": "U123ABC456",
            "channel": "C123ABC456",
            "text": "hello",
            "ts": "1515449522.000016",
            "event_ts": "1515449522000016"
        }
    });
    let mut raw = raw_inbound(&payload, 1_715_000_000);
    raw.headers.insert(
        "X-Slack-Signature".to_string(),
        "v0=0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    );
    let error = connector.normalize_inbound(raw).unwrap_err();
    assert!(matches!(
        error,
        crate::connectors::ConnectorError::InvalidSignature(_)
    ));
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Default)]
struct MockScenario {
    requests: Vec<CapturedRequest>,
}

fn accept_with_deadline(listener: &TcpListener, label: &str) -> std::net::TcpStream {
    listener.set_nonblocking(true).expect("set nonblocking");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("restore blocking mode");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .ok();
                stream
                    .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                    .ok();
                return stream;
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    panic!("{label}: no client within 3s");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
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
    let request_line = lines.next().expect("request line");
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

fn write_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    headers: &[(&str, String)],
    body: &str,
) {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        _ => "OK",
    };
    let mut response = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        status,
        status_text,
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    response.push_str(body);
    stream
        .write_all(response.as_bytes())
        .expect("write response");
}

fn spawn_mock_server(
    expected_requests: usize,
    scenario: Arc<Mutex<MockScenario>>,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let handle = std::thread::spawn(move || {
        for _ in 0..expected_requests {
            let mut stream = accept_with_deadline(&listener, "slack mock server");
            let request = read_request(&mut stream);
            scenario
                .lock()
                .expect("scenario lock")
                .requests
                .push(request.clone());
            match request.path.as_str() {
                "/chat.postMessage" => write_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"channel":"C123ABC456","ts":"1715.000100","message":{"text":"hello from harn"}}"#,
                ),
                "/chat.update" => write_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"channel":"C123ABC456","ts":"1715.000100","text":"updated"}"#,
                ),
                "/reactions.add" => write_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true}"#,
                ),
                "/files.getUploadURLExternal" => write_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    &format!(
                        "{{\"ok\":true,\"upload_url\":\"http://{}/upload/F123\",\"file_id\":\"F123\"}}",
                        addr
                    ),
                ),
                "/upload/F123" => write_response(&mut stream, 200, &[], ""),
                "/files.completeUploadExternal" => write_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"files":[{"id":"F123","title":"notes.txt"}]}"#,
                ),
                other => panic!("unexpected path {other}"),
            }
        }
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn slack_outbound_helpers_hit_expected_api_methods() {
    let client = initialized_client().await;
    let scenario = Arc::new(Mutex::new(MockScenario::default()));
    let (base_url, handle) = spawn_mock_server(6, scenario.clone());

    let posted = client
        .call(
            "post_message",
            json!({
                "api_base_url": base_url,
                "bot_token_secret": "slack/bot-token",
                "channel": "C123ABC456",
                "text": "hello from harn",
            }),
        )
        .await
        .unwrap();
    let updated = client
        .call(
            "update_message",
            json!({
                "api_base_url": base_url,
                "bot_token": BOT_TOKEN,
                "channel": "C123ABC456",
                "ts": "1715.000100",
                "text": "updated",
            }),
        )
        .await
        .unwrap();
    client
        .call(
            "add_reaction",
            json!({
                "api_base_url": base_url,
                "bot_token": BOT_TOKEN,
                "channel": "C123ABC456",
                "ts": "1715.000100",
                "name": "thumbsup",
            }),
        )
        .await
        .unwrap();
    let uploaded = client
        .call(
            "upload_file",
            json!({
                "api_base_url": base_url,
                "bot_token": BOT_TOKEN,
                "filename": "notes.txt",
                "content": "hello upload",
                "title": "notes.txt",
                "channel_id": "C123ABC456",
            }),
        )
        .await
        .unwrap();

    handle.join().expect("server thread");
    let requests = scenario.lock().expect("scenario lock").requests.clone();
    assert_eq!(requests.len(), 6);
    assert!(requests.iter().all(|request| request.method == "POST"));
    assert_eq!(requests[0].path, "/chat.postMessage");
    assert_eq!(requests[1].path, "/chat.update");
    assert_eq!(requests[2].path, "/reactions.add");
    assert_eq!(requests[3].path, "/files.getUploadURLExternal");
    assert_eq!(requests[4].path, "/upload/F123");
    assert_eq!(requests[5].path, "/files.completeUploadExternal");
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("Bearer xoxb-test-token")
    );
    assert!(requests[3].body.contains("filename=notes.txt"));
    assert_eq!(requests[4].body, "hello upload");
    assert!(requests[5].body.contains("\"channel_id\":\"C123ABC456\""));
    assert_eq!(posted["channel"], json!("C123ABC456"));
    assert_eq!(updated["text"], json!("updated"));
    assert_eq!(uploaded["file_id"], json!("F123"));
}
