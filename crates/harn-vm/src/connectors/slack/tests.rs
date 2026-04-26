use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::connectors::{
    test_util::{
        accept_http_connection, read_http_request, write_http_response,
        CapturedHttpRequest as CapturedRequest,
    },
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
                "event_time": 1515449522
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
                "event_time": 1465244570
            }),
        },
        FixtureCase {
            name: "app_home_opened",
            expected_kind: "app_home_opened",
            body: json!({
                "token": "z26uFbvR1xHJEdHE1OQiO6t8",
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "app_home_opened",
                    "user": "U123ABC456",
                    "channel": "D123ABC456",
                    "event_ts": "1515449522000016",
                    "tab": "home",
                    "view": {
                        "id": "V123ABC456",
                        "team_id": "T123ABC456",
                        "type": "home"
                    }
                },
                "type": "event_callback",
                "event_id": "Ev123HOME",
                "event_time": 1515449522
            }),
        },
        FixtureCase {
            name: "assistant_thread_started",
            expected_kind: "assistant_thread_started",
            body: json!({
                "token": "z26uFbvR1xHJEdHE1OQiO6t8",
                "team_id": "T07XY8FPJ5C",
                "api_app_id": "A123ABC456",
                "event": {
                    "type": "assistant_thread_started",
                    "assistant_thread": {
                        "user_id": "U123ABC456",
                        "context": {
                            "channel_id": "C123ABC456",
                            "team_id": "T07XY8FPJ5C",
                            "enterprise_id": "E480293PS82"
                        },
                        "channel_id": "D123ABC456",
                        "thread_ts": "1729999327.187299"
                    },
                    "event_ts": "1715873754.429808"
                },
                "type": "event_callback",
                "event_id": "Ev123ASSISTANT",
                "event_time": 1715873754
            }),
        },
    ];

    for case in cases {
        let event = connector
            .normalize_inbound(raw_inbound(&case.body, timestamp))
            .await
            .unwrap();
        assert_eq!(event.kind, case.expected_kind, "case {}", case.name);
        let payload = match &event.provider_payload {
            crate::triggers::ProviderPayload::Known(KnownProviderPayload::Slack(payload)) => {
                payload
            }
            other => panic!("expected slack payload, got {other:?}"),
        };
        match (case.expected_kind, payload.as_ref()) {
            ("message.channels", SlackEventPayload::Message(inner)) => {
                assert_eq!(inner.channel.as_deref(), Some("C123ABC456"));
                assert_eq!(inner.channel_type.as_deref(), Some("channel"));
            }
            ("app_mention", SlackEventPayload::AppMention(inner)) => {
                assert_eq!(inner.user.as_deref(), Some("U123ABC456"));
            }
            ("reaction_added", SlackEventPayload::ReactionAdded(inner)) => {
                assert_eq!(inner.reaction.as_deref(), Some("slightly_smiling_face"));
            }
            ("app_home_opened", SlackEventPayload::AppHomeOpened(inner)) => {
                assert_eq!(inner.channel.as_deref(), Some("D123ABC456"));
                assert_eq!(inner.tab.as_deref(), Some("home"));
            }
            ("assistant_thread_started", SlackEventPayload::AssistantThreadStarted(inner)) => {
                assert_eq!(inner.common.channel_id.as_deref(), Some("D123ABC456"));
                assert_eq!(inner.common.user_id.as_deref(), Some("U123ABC456"));
                assert_eq!(inner.thread_ts.as_deref(), Some("1729999327.187299"));
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
        .await
        .unwrap();
    assert_eq!(event.kind, "url_verification");
    match event.provider_payload {
        crate::triggers::ProviderPayload::Known(KnownProviderPayload::Slack(payload)) => {
            let SlackEventPayload::Other(common) = *payload else {
                panic!("expected slack other payload");
            };
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
    let error = connector.normalize_inbound(raw).await.unwrap_err();
    assert!(matches!(
        error,
        crate::connectors::ConnectorError::InvalidSignature(_)
    ));
}

#[derive(Default)]
struct MockScenario {
    requests: Vec<CapturedRequest>,
}

fn spawn_mock_server(
    expected_requests: usize,
    scenario: Arc<Mutex<MockScenario>>,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let handle = std::thread::spawn(move || {
        for _ in 0..expected_requests {
            let mut stream = accept_http_connection(&listener, "slack mock server");
            let request = read_http_request(&mut stream);
            scenario
                .lock()
                .expect("scenario lock")
                .requests
                .push(request.clone());
            match request.path.as_str() {
                "/chat.postMessage" => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"channel":"C123ABC456","ts":"1715.000100","message":{"text":"hello from harn"}}"#,
                ),
                "/chat.update" => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"channel":"C123ABC456","ts":"1715.000100","text":"updated"}"#,
                ),
                "/reactions.add" => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true}"#,
                ),
                "/views.open" => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"view":{"id":"V123ABC456","type":"modal"}}"#,
                ),
                path if path.starts_with("/users.info") => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"user":{"id":"U123ABC456","name":"roadrunner"}}"#,
                ),
                "/auth.test" => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    r#"{"ok":true,"url":"https://example.slack.com/","team":"Example","user":"bot"}"#,
                ),
                "/files.getUploadURLExternal" => write_http_response(
                    &mut stream,
                    200,
                    &[("content-type", "application/json".to_string())],
                    &format!(
                        "{{\"ok\":true,\"upload_url\":\"http://{}/upload/F123\",\"file_id\":\"F123\"}}",
                        addr
                    ),
                ),
                "/upload/F123" => write_http_response(&mut stream, 200, &[], ""),
                "/files.completeUploadExternal" => write_http_response(
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

// Holding the std egress test mutex across `.await` is intentional:
// the lock simply serializes whole-test bodies against the
// egress::tests suite (uncontended outside tests).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn slack_outbound_helpers_hit_expected_api_methods() {
    let _egress_guard = crate::egress::egress_test_guard();
    let client = initialized_client().await;
    let scenario = Arc::new(Mutex::new(MockScenario::default()));
    let (base_url, handle) = spawn_mock_server(9, scenario.clone());

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
    let opened = client
        .call(
            "open_view",
            json!({
                "api_base_url": base_url,
                "bot_token": BOT_TOKEN,
                "trigger_id": "12345.98765.abcd2358fdea",
                "view": {
                    "type": "modal",
                    "title": {"type": "plain_text", "text": "Status"},
                    "close": {"type": "plain_text", "text": "Close"},
                    "blocks": []
                }
            }),
        )
        .await
        .unwrap();
    let user = client
        .call(
            "user_info",
            json!({
                "api_base_url": base_url,
                "bot_token": BOT_TOKEN,
                "user_id": "U123ABC456",
                "include_locale": true,
            }),
        )
        .await
        .unwrap();
    let auth = client
        .call(
            "api_call",
            json!({
                "api_base_url": base_url,
                "bot_token": BOT_TOKEN,
                "method": "auth.test",
                "args": {}
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
    assert_eq!(requests.len(), 9);
    assert_eq!(requests[0].path, "/chat.postMessage");
    assert_eq!(requests[1].path, "/chat.update");
    assert_eq!(requests[2].path, "/reactions.add");
    assert_eq!(requests[3].path, "/views.open");
    assert!(requests[4].path.starts_with("/users.info?"));
    assert_eq!(requests[4].method, "GET");
    assert!(requests[4].path.contains("user=U123ABC456"));
    assert!(requests[4].path.contains("include_locale=true"));
    assert_eq!(requests[5].path, "/auth.test");
    assert_eq!(requests[6].path, "/files.getUploadURLExternal");
    assert_eq!(requests[7].path, "/upload/F123");
    assert_eq!(requests[8].path, "/files.completeUploadExternal");
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("Bearer xoxb-test-token")
    );
    assert!(requests[3]
        .body
        .contains("\"trigger_id\":\"12345.98765.abcd2358fdea\""));
    assert!(requests[6].body.contains("filename=notes.txt"));
    assert_eq!(requests[7].body, "hello upload");
    assert!(requests[8].body.contains("\"channel_id\":\"C123ABC456\""));
    assert_eq!(posted["channel"], json!("C123ABC456"));
    assert_eq!(updated["text"], json!("updated"));
    assert_eq!(opened["view"]["id"], json!("V123ABC456"));
    assert_eq!(user["user"]["id"], json!("U123ABC456"));
    assert_eq!(auth["team"], json!("Example"));
    assert_eq!(uploaded["file_id"], json!("F123"));
}
