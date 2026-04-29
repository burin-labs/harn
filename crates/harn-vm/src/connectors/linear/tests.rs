use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::connectors::{
    test_util::{spawn_mock_http_server, MockHttpServer},
    Connector, ConnectorClient, ConnectorCtx, InboxIndex, MetricsRegistry, RateLimiterFactory,
    RawInbound, TriggerBinding,
};
use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::secrets::{SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider};
use crate::triggers::event::{KnownProviderPayload, LinearEventPayload, LinearIssueChange};
use crate::triggers::ProviderId;

const SIGNING_SECRET: &str = "linear-signing-secret";
const ACCESS_TOKEN: &str = "linear-access-token";

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

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Option<JsonValue>,
}

async fn test_ctx(secrets: Arc<dyn SecretProvider>) -> ConnectorCtx {
    let event_log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
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

async fn connector() -> (LinearConnector, Arc<MetricsRegistry>) {
    let secrets = Arc::new(StaticSecretProvider::new(
        "linear",
        BTreeMap::from([
            (
                SecretId::new("linear", "test-signing-secret"),
                SIGNING_SECRET.to_string(),
            ),
            (
                SecretId::new("linear", "access-token"),
                ACCESS_TOKEN.to_string(),
            ),
        ]),
    ));
    let ctx = test_ctx(secrets).await;
    let metrics = ctx.metrics.clone();
    let mut connector = LinearConnector::new();
    connector.init(ctx).await.unwrap();
    connector.activate(&[binding()]).await.unwrap();
    (connector, metrics)
}

async fn initialized_client(api_base_url: &str) -> Arc<dyn ConnectorClient> {
    let (connector, _) = connector().await;
    let client = connector.client();
    let _ = api_base_url;
    client
}

async fn connector_with_binding(
    binding: TriggerBinding,
) -> (LinearConnector, Arc<MetricsRegistry>) {
    let secrets = Arc::new(StaticSecretProvider::new(
        "linear",
        BTreeMap::from([
            (
                SecretId::new("linear", "test-signing-secret"),
                SIGNING_SECRET.to_string(),
            ),
            (
                SecretId::new("linear", "access-token"),
                ACCESS_TOKEN.to_string(),
            ),
        ]),
    ));
    let ctx = test_ctx(secrets).await;
    let metrics = ctx.metrics.clone();
    let mut connector = LinearConnector::new();
    connector.init(ctx).await.unwrap();
    connector.activate(&[binding]).await.unwrap();
    (connector, metrics)
}

fn binding() -> TriggerBinding {
    let mut binding = TriggerBinding::new(ProviderId::from("linear"), "webhook", "linear.test");
    binding.config = json!({
        "match": { "path": "/hooks/linear" },
        "replay_grace_secs": 15,
        "secrets": {
            "signing_secret": "linear/test-signing-secret",
            "access_token": "linear/access-token"
        },
    });
    binding
}

fn raw_inbound(body: &JsonValue, received_at_ms: i64) -> RawInbound {
    let encoded = serde_json::to_vec(body).unwrap();
    let headers = linear_headers(&encoded, "delivery-123");
    let mut raw = RawInbound::new("", headers, encoded);
    raw.received_at = OffsetDateTime::from_unix_timestamp(received_at_ms / 1000).unwrap();
    raw.metadata = json!({ "binding_id": "linear.test" });
    raw
}

fn linear_headers(body: &[u8], delivery_id: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "Linear-Signature".to_string(),
            hex::encode(hmac_sha256(SIGNING_SECRET.as_bytes(), body)),
        ),
        ("Linear-Delivery".to_string(), delivery_id.to_string()),
    ])
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
    body: JsonValue,
    expected_kind: &'static str,
}

#[tokio::test]
async fn linear_connector_normalizes_typed_variants() {
    let (connector, _) = connector().await;
    let received_at_ms = 1_715_000_000_000i64;
    let cases = vec![
        FixtureCase {
            expected_kind: "issue.update",
            body: json!({
                "action": "update",
                "type": "Issue",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "webhookId": "wh_123",
                "createdAt": "2026-04-19T00:00:00Z",
                "actor": { "id": "user_1", "name": "Ada" },
                "data": { "id": "ISS-1", "title": "Fix Linear connector" },
                "updatedFrom": { "title": "Previous title", "priority": 2, "labelIds": ["lbl_1"] }
            }),
        },
        FixtureCase {
            expected_kind: "comment.create",
            body: json!({
                "action": "create",
                "type": "Comment",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "actor": { "id": "user_1" },
                "data": { "id": "COM-1", "body": "hello" }
            }),
        },
        FixtureCase {
            expected_kind: "issue_label.update",
            body: json!({
                "action": "update",
                "type": "IssueLabel",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "actor": { "id": "user_1" },
                "data": { "id": "LBL-1", "name": "bug" }
            }),
        },
        FixtureCase {
            expected_kind: "project.update",
            body: json!({
                "action": "update",
                "type": "Project",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "actor": { "id": "user_1" },
                "data": { "id": "PRJ-1", "name": "Linear MVP" }
            }),
        },
        FixtureCase {
            expected_kind: "cycle.update",
            body: json!({
                "action": "update",
                "type": "Cycle",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "actor": { "id": "user_1" },
                "data": { "id": "CYC-1", "name": "Cycle 1" }
            }),
        },
        FixtureCase {
            expected_kind: "customer.update",
            body: json!({
                "action": "update",
                "type": "Customer",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "actor": { "id": "user_1" },
                "data": { "id": "CUS-1", "name": "Acme" }
            }),
        },
        FixtureCase {
            expected_kind: "customer_request.create",
            body: json!({
                "action": "create",
                "type": "CustomerRequest",
                "organizationId": "org_123",
                "webhookTimestamp": received_at_ms,
                "actor": { "id": "user_1" },
                "data": { "id": "REQ-1", "title": "Need this shipped" }
            }),
        },
    ];

    for case in cases {
        let event = connector
            .normalize_inbound(raw_inbound(&case.body, received_at_ms))
            .await
            .expect("normalize linear event");
        assert_eq!(event.kind, case.expected_kind);
        assert_eq!(event.signature_status, SignatureStatus::Verified);
        match &event.provider_payload {
            ProviderPayload::Known(KnownProviderPayload::Linear(LinearEventPayload::Issue(
                value,
            ))) => {
                assert_eq!(value.issue["id"], "ISS-1");
                assert_eq!(value.changes.len(), 3);
                assert!(value.changes.iter().any(|change| matches!(
                    change,
                    LinearIssueChange::Title { previous: Some(_) }
                )));
            }
            ProviderPayload::Known(KnownProviderPayload::Linear(
                LinearEventPayload::IssueComment(value),
            )) => {
                assert_eq!(value.comment["id"], "COM-1");
            }
            ProviderPayload::Known(KnownProviderPayload::Linear(
                LinearEventPayload::IssueLabel(value),
            )) => {
                assert_eq!(value.label["id"], "LBL-1");
            }
            ProviderPayload::Known(KnownProviderPayload::Linear(LinearEventPayload::Project(
                value,
            ))) => {
                assert_eq!(value.project["id"], "PRJ-1");
            }
            ProviderPayload::Known(KnownProviderPayload::Linear(LinearEventPayload::Cycle(
                value,
            ))) => {
                assert_eq!(value.cycle["id"], "CYC-1");
            }
            ProviderPayload::Known(KnownProviderPayload::Linear(LinearEventPayload::Customer(
                value,
            ))) => {
                assert_eq!(value.customer["id"], "CUS-1");
            }
            ProviderPayload::Known(KnownProviderPayload::Linear(
                LinearEventPayload::CustomerRequest(value),
            )) => {
                assert_eq!(value.customer_request["id"], "REQ-1");
            }
            other => panic!("unexpected payload {other:?}"),
        }
    }
}

#[tokio::test]
async fn linear_connector_rejects_stale_timestamps_and_records_metric() {
    let (connector, metrics) = connector().await;
    let payload = json!({
        "action": "update",
        "type": "Issue",
        "organizationId": "org_123",
        "webhookTimestamp": 1_715_000_000_000i64,
        "actor": { "id": "user_1" },
        "data": { "id": "ISS-1", "title": "stale" }
    });
    let error = connector
        .normalize_inbound(raw_inbound(&payload, 1_715_000_100_000i64))
        .await
        .expect_err("stale timestamp should reject");
    assert!(matches!(error, ConnectorError::TimestampOutOfWindow { .. }));
    assert_eq!(metrics.snapshot().linear_timestamp_rejections_total, 1);
}

#[tokio::test]
async fn linear_client_supports_typed_methods_and_escape_hatch() {
    let requests = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_mock_server(requests.clone(), 5);
    let base_url = server.base_url().to_string();
    let client = initialized_client(&base_url).await;

    let list = client
        .call(
            "list_issues",
            json!({
                "api_base_url": base_url,
                "access_token": ACCESS_TOKEN,
                "filter": { "priority": { "lte": 2 } },
                "first": 10
            }),
        )
        .await
        .expect("list issues");
    let updated = client
        .call(
            "update_issue",
            json!({
                "api_base_url": base_url,
                "access_token": ACCESS_TOKEN,
                "id": "ISS-1",
                "changes": { "title": "Updated title" }
            }),
        )
        .await
        .expect("update issue");
    let comment = client
        .call(
            "create_comment",
            json!({
                "api_base_url": base_url,
                "access_token": ACCESS_TOKEN,
                "issue_id": "ISS-1",
                "body": "Looks good"
            }),
        )
        .await
        .expect("create comment");
    let search = client
        .call(
            "search",
            json!({
                "api_base_url": base_url,
                "access_token": ACCESS_TOKEN,
                "query": "connector",
                "first": 5
            }),
        )
        .await
        .expect("search");
    let graphql = client
        .call(
            "graphql",
            json!({
                "api_base_url": base_url,
                "access_token": ACCESS_TOKEN,
                "query": "query Viewer { viewer { id } }",
                "operation_name": "Viewer"
            }),
        )
        .await
        .expect("graphql");

    drop(server);
    let requests = requests.lock().expect("requests lock").clone();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .is_some_and(|value| value == &format!("Bearer {ACCESS_TOKEN}"))
    }));
    assert!(requests[0].body.as_ref().unwrap()["query"]
        .as_str()
        .unwrap()
        .contains("issues("));
    assert!(requests[1].body.as_ref().unwrap()["query"]
        .as_str()
        .unwrap()
        .contains("issueUpdate"));
    assert!(requests[2].body.as_ref().unwrap()["query"]
        .as_str()
        .unwrap()
        .contains("commentCreate"));
    assert!(requests[3].body.as_ref().unwrap()["query"]
        .as_str()
        .unwrap()
        .contains("searchIssues"));
    assert_eq!(
        requests[4].body.as_ref().unwrap()["operationName"],
        "Viewer"
    );

    assert_eq!(list["nodes"][0]["identifier"], "ENG-1");
    assert_eq!(updated["success"], true);
    assert_eq!(comment["comment"]["id"], "COM-1");
    assert_eq!(search["nodes"][0]["identifier"], "ENG-1");
    assert_eq!(graphql["data"]["viewer"]["id"], "user-1");
    assert_eq!(graphql["meta"]["observed_complexity"], 12);
}

#[tokio::test]
async fn linear_connector_monitor_reenables_webhook_after_probe_streak() {
    let requests = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let mut server = spawn_monitor_server(requests.clone(), 3);
    let base_url = server.base_url().to_string();
    let mut binding = binding();
    binding.config = json!({
        "match": { "path": "/hooks/linear" },
        "replay_grace_secs": 15,
        "secrets": {
            "signing_secret": "linear/test-signing-secret",
            "access_token": "linear/access-token"
        },
        "monitor": {
            "webhook_id": "wh-123",
            "health_url": format!("{base_url}/health"),
            "api_base_url": base_url,
            "probe_interval_ms": 25,
            "success_threshold": 2
        }
    });
    let (connector, _) = connector_with_binding(binding).await;

    // Block until the mock server has served all three expected probes and
    // its worker thread exits, instead of polling a shared counter against
    // a wall-clock deadline. nextest's per-test timeout will surface a
    // genuine deadlock; we no longer pretend to bound it ourselves.
    tokio::task::spawn_blocking(move || {
        server.wait_until_handled();
    })
    .await
    .expect("await monitor server completion");
    connector
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .expect("shutdown connector");

    let requests = requests.lock().expect("requests lock").clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/health");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/health");
    assert_eq!(requests[2].method, "POST");
    assert_eq!(requests[2].path, "/");
    assert!(requests[2]
        .headers
        .get("authorization")
        .is_some_and(|value| { value == &format!("Bearer {ACCESS_TOKEN}") }));
    assert!(requests[2].body.as_ref().unwrap()["query"]
        .as_str()
        .unwrap()
        .contains("webhookUpdate"));
    assert_eq!(
        requests[2].body.as_ref().unwrap()["variables"]["id"],
        "wh-123"
    );
    assert_eq!(
        requests[2].body.as_ref().unwrap()["variables"]["input"]["enabled"],
        JsonValue::Bool(true)
    );
}

fn capture_request(
    requests: &Arc<Mutex<Vec<CapturedRequest>>>,
    raw: &crate::connectors::test_util::CapturedHttpRequest,
) -> CapturedRequest {
    let body = if raw.body.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&raw.body).expect("decode request body"))
    };
    let captured = CapturedRequest {
        method: raw.method.clone(),
        path: raw.path.clone(),
        headers: raw.headers.clone(),
        body,
    };
    requests
        .lock()
        .expect("requests lock")
        .push(captured.clone());
    captured
}

fn spawn_mock_server(
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    expected_requests: usize,
) -> MockHttpServer {
    spawn_mock_http_server(
        expected_requests,
        "linear mock server",
        move |_index, _addr, raw, stream| {
            let request = capture_request(&requests, raw);
            let query = request
                .body
                .as_ref()
                .and_then(|body| body.get("query"))
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            let body = if query.contains("issueUpdate") {
                json!({
                    "data": {
                        "issueUpdate": {
                            "success": true,
                            "issue": { "id": "ISS-1", "identifier": "ENG-1", "title": "Updated title" }
                        }
                    }
                })
            } else if query.contains("commentCreate") {
                json!({
                    "data": {
                        "commentCreate": {
                            "success": true,
                            "comment": { "id": "COM-1", "body": "Looks good" }
                        }
                    }
                })
            } else if query.contains("searchIssues") {
                json!({
                    "data": {
                        "searchIssues": {
                            "nodes": [{ "id": "ISS-1", "identifier": "ENG-1", "title": "connector" }]
                        }
                    }
                })
            } else if query.contains("viewer") {
                json!({
                    "data": {
                        "viewer": { "id": "user-1" }
                    }
                })
            } else {
                json!({
                    "data": {
                        "issues": {
                            "nodes": [{ "id": "ISS-1", "identifier": "ENG-1", "title": "Connector issue" }],
                            "pageInfo": { "hasNextPage": false, "endCursor": JsonValue::Null }
                        }
                    }
                })
            };
            write_response(
                stream,
                &body.to_string(),
                &[("x-complexity", "12"), ("content-type", "application/json")],
            );
        },
    )
}

fn write_response(stream: &mut std::net::TcpStream, body: &str, headers: &[(&str, &str)]) {
    write_response_status(stream, "200 OK", body, headers);
}

fn write_response_status(
    stream: &mut std::net::TcpStream,
    status: &str,
    body: &str,
    headers: &[(&str, &str)],
) {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
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

fn spawn_monitor_server(
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    expected_requests: usize,
) -> MockHttpServer {
    spawn_mock_http_server(
        expected_requests,
        "linear monitor server",
        move |_index, _addr, raw, stream| {
            let request = capture_request(&requests, raw);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/health") => {
                    write_response_status(stream, "200 OK", "", &[("content-type", "text/plain")]);
                }
                ("POST", "/") => {
                    let query = request
                        .body
                        .as_ref()
                        .and_then(|body| body.get("query"))
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default();
                    assert!(
                        query.contains("webhookUpdate"),
                        "unexpected GraphQL query: {query}"
                    );
                    write_response(
                        stream,
                        &json!({
                            "data": {
                                "webhookUpdate": {
                                    "success": true,
                                    "webhook": { "id": "wh-123", "enabled": true }
                                }
                            }
                        })
                        .to_string(),
                        &[("content-type", "application/json")],
                    );
                }
                other => panic!("unexpected request {other:?}"),
            }
        },
    )
}
