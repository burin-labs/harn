use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::connectors::{
    test_util::{
        spawn_mock_http_server, write_http_response, CapturedHttpRequest as CapturedRequest,
        MockHttpServer,
    },
    Connector, ConnectorCtx, InboxIndex, MetricsRegistry, RateLimiterFactory, RawInbound,
    TriggerBinding,
};
use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::secrets::{SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider};

const TEST_PRIVATE_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQD6D+dF57y94Dnt
0MO/4xXRc7dhwXB2EGNngx3ln9XpamhJEtBHJ0n89cp047g2W/nX26BHlEjFh7Kp
zeRzWARhNL9y39M4sRdHyP28O85dBElaTAAk520AXFhLHF2v8+4pJSDs78toSJoR
fFNWmfeFYsFzAk4y8gnocYJlh/1YkGPEoWlvsDlqtJlvE0mp8sNq5Bii39tOEZ2E
QGlWFgG+vqiVfkQ440vqHbMArWfuZPT7M7EKwqHa87ktFQYqex9vTvhnt+7NtE2x
pjxDW+Tvm+80q6l3rBiOBvNosljuX0YR1awr9xCtybIGej3Ja+NT4mkTNREm9xE7
rZGPqUZ7AgMBAAECggEAAYeHtGuVQcYKz1O3tsc78sLV7CD46p1Gtl3cw3LwUH6o
R6DNVE9ptO8zP0wbQX3bhStLNioygxQa5CNQZ+Ixw1RwF/2azEh3qiaRDWMs68W4
cHccMx2VPZYoVhaaKMGBreUTvU8+0RL3RO8b5WDeB4X8mpL43nfmK/KccmQxXlF0
qnAvP5ttr8jBAJUH4rfkeJMe8XKGuZ3/96bmgs3ECxLqrPeoO7l/DL0DRGzV5k4q
EKAysy5Dme0Nv5s9QOFEVO6QsTq27r68FCH13mvpD+97l+0n12YagxoV1Q1IQ5nP
0emZn+p5NMwPnRsS9LHykPdxDkefGgx0NBzJT3QoUQKBgQD9xwMU9bGpf14wRDLr
KJKYCrCMhQHyi4rUs33Lc67K5+EJASHJupEPHRY3/f8Y6ooMgR7m1CEGMQHnUdJl
Q2/sFrTeecodQdJKTcVSeh1MTXswD0aLkEdVqkppfy7T4qoZ2ppW4LUiX5jk5JGC
l2lWriT7RWIe0IWDXyxaOlI0qwKBgQD8QI+797MC9PSeLOC1hk6ysDBy87vzGY+r
+nbv0q97R4KPXhMdv76i1pG/tQRSVNGfsjj8bSdxLWlaN3MLRXKHoUNgNTM5P+F8
tzMxtjm1yoxYDxhpcenf0V2hxlBUsB3b0CNAvWkGS62cl/FrnZS+hLyrZpDQRei3
whzjTSoVcQKBgQDcGKkEmZ4vOebvh4Z9yx9wu/yosoag3ANZPB7CwB79naPfUlsC
gUtjxz9I6oI/EtMNy0KIwbuuifxzqdQGvTkpkfvl48y2GSsQBGk5ge09CwnnAaiW
TFiB5IJLAuITJEeQyrYG2TZfjHenNNE6aKUUZ05tmpxhy0mwSW/HBUPcpwKBgQDj
XVHwy9fXX3EpLSwxkehXWUWiJxyOhsif67bOfWlcRd1hWhsC4nRzE9H1KLTHfNoh
BiQlKkG12oeuIHKagzMzGuC+09Ti0jhtEDedpDEqMXIEYT7QtDNoYK7zhOudGc0f
9t//l3oViZrnnXCmXjfW7Y+dMmpuv8R99QHSwxeekQKBgQDGPfhwQemwlxgJCkJN
oKIjyfNccGU/D862zin4ljL6i1K//ZyPcoVcBjK3TsJfvirlHT2NrC7NPFhVxTsv
hrrOOT1eho/B+Aa3c0qI31mtPblqn8E0xNfmnItXsOhyAoHo94KkiHbYmzryimci
wW5HJa11Ik9Dswps8BdY31/K6Q==
-----END PRIVATE KEY-----"#;

struct StaticSecretProvider {
    namespace: String,
    secrets: BTreeMap<SecretId, String>,
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

#[derive(Default)]
struct MockScenario {
    token_requests: usize,
    api_requests: Vec<CapturedRequest>,
    unauthorized_once: HashSet<String>,
    rate_limit_once: HashSet<String>,
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

async fn initialized_client(secrets: Arc<dyn SecretProvider>) -> Arc<dyn ConnectorClient> {
    let mut connector = GitHubConnector::new();
    connector.init(test_ctx(secrets).await).await.unwrap();
    connector.client()
}

fn client_args(api_base_url: &str) -> JsonValue {
    json!({
        "app_id": 123,
        "installation_id": 77,
        "api_base_url": api_base_url,
        "private_key_pem": TEST_PRIVATE_KEY_PEM,
    })
}

fn spawn_mock_server(
    expected_requests: usize,
    scenario: Arc<Mutex<MockScenario>>,
) -> MockHttpServer {
    spawn_mock_http_server(
        expected_requests,
        "github mock server",
        move |_index, _addr, request, stream| {
            let response = {
                let mut state = scenario.lock().expect("scenario lock");
                if request.path == "/app/installations/77/access_tokens" {
                    state.token_requests += 1;
                    let token = format!("token-{}", state.token_requests);
                    (
                        201,
                        vec![("content-type", "application/json".to_string())],
                        json!({
                            "token": token,
                            "expires_at": "2030-01-01T00:00:00Z",
                        })
                        .to_string(),
                    )
                } else if state.unauthorized_once.remove(&request.path) {
                    (
                        401,
                        vec![("content-type", "application/json".to_string())],
                        json!({"message": "expired token"}).to_string(),
                    )
                } else if state.rate_limit_once.remove(&request.path) {
                    (
                        429,
                        vec![
                            ("content-type", "application/json".to_string()),
                            ("retry-after", "0".to_string()),
                        ],
                        json!({"message": "slow down"}).to_string(),
                    )
                } else {
                    state.api_requests.push(request.clone());
                    let accept = request.headers.get("accept").cloned().unwrap_or_default();
                    let (status, body) = if request.path.ends_with("/comments") {
                        (201, json!({"id": 1, "body": "commented"}).to_string())
                    } else if request.path.ends_with("/labels") {
                        (
                            200,
                            json!([{"name": "bug"}, {"name": "triage"}]).to_string(),
                        )
                    } else if request.path.ends_with("/requested_reviewers") {
                        (201, json!({"requested_reviewers": ["alice"]}).to_string())
                    } else if request.path.ends_with("/merge") {
                        (
                            200,
                            json!({"merged": true, "message": "merged"}).to_string(),
                        )
                    } else if request.path.starts_with("/search/issues") {
                        (
                            200,
                            json!({"total_count": 1, "items": [{"number": 7, "title": "stale"}]})
                                .to_string(),
                        )
                    } else if request.path.ends_with("/pulls/123")
                        && accept.contains("application/vnd.github.diff")
                    {
                        (200, "diff --git a/file b/file\n".to_string())
                    } else if request.path.ends_with("/issues") {
                        (201, json!({"number": 88, "title": "created"}).to_string())
                    } else {
                        (200, json!({"ok": true}).to_string())
                    };
                    let content_type = if accept.contains("application/vnd.github.diff") {
                        "text/plain".to_string()
                    } else {
                        "application/json".to_string()
                    };
                    (
                        status,
                        vec![
                            ("content-type", content_type),
                            ("x-ratelimit-remaining", "4999".to_string()),
                        ],
                        body,
                    )
                }
            };
            write_http_response(stream, response.0, &response.1, &response.2);
        },
    )
}

fn github_signature(secret: &str, body: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut key = secret.as_bytes().to_vec();
    if key.len() > BLOCK {
        key = Sha256::digest(&key).to_vec();
    }
    key.resize(BLOCK, 0);
    let mut inner_pad = vec![0x36; BLOCK];
    let mut outer_pad = vec![0x5c; BLOCK];
    for i in 0..BLOCK {
        inner_pad[i] ^= key[i];
        outer_pad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(&inner_pad);
    inner.update(body);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&outer_pad);
    outer.update(inner_digest);
    format!("sha256={}", hex::encode(outer.finalize()))
}

#[test]
fn parses_all_supported_github_event_variants() {
    assert!(matches!(
        parse_typed_event(
            "issues",
            &json!({"action": "opened", "issue": {"number": 1}})
        )
        .unwrap(),
        ParsedGitHubEvent::Issues(_)
    ));
    assert!(matches!(
        parse_typed_event(
            "pull_request",
            &json!({"action": "opened", "pull_request": {"number": 2}})
        )
        .unwrap(),
        ParsedGitHubEvent::PullRequest(_)
    ));
    assert!(matches!(
        parse_typed_event(
            "issue_comment",
            &json!({"action": "created", "comment": {"id": 3}, "issue": {"number": 2}})
        )
        .unwrap(),
        ParsedGitHubEvent::IssueComment(_)
    ));
    assert!(matches!(
        parse_typed_event(
            "pull_request_review",
            &json!({"action": "submitted", "review": {"id": 4}, "pull_request": {"number": 2}})
        )
        .unwrap(),
        ParsedGitHubEvent::PullRequestReview(_)
    ));
    assert!(matches!(
        parse_typed_event("push", &json!({"commits": []})).unwrap(),
        ParsedGitHubEvent::Push(_)
    ));
    assert!(matches!(
        parse_typed_event(
            "workflow_run",
            &json!({"action": "completed", "workflow_run": {"id": 5}})
        )
        .unwrap(),
        ParsedGitHubEvent::WorkflowRun(_)
    ));
    assert!(matches!(
        parse_typed_event(
            "deployment_status",
            &json!({
                "action": "created",
                "deployment_status": {"id": 6, "state": "in_progress"},
                "deployment": {"id": 16, "environment": "staging"}
            })
        )
        .unwrap(),
        ParsedGitHubEvent::DeploymentStatus(_)
    ));
    assert!(matches!(
        parse_typed_event(
            "check_run",
            &json!({
                "action": "completed",
                "check_run": {"id": 7, "status": "completed", "conclusion": "success"}
            })
        )
        .unwrap(),
        ParsedGitHubEvent::CheckRun(_)
    ));
}

#[tokio::test]
async fn normalizes_signed_github_webhook_events() {
    let secrets = Arc::new(StaticSecretProvider {
        namespace: "github".to_string(),
        secrets: BTreeMap::from([(
            SecretId::new("github", "webhook-secret"),
            "topsecret".to_string(),
        )]),
    });
    let mut connector = GitHubConnector::new();
    connector.init(test_ctx(secrets).await).await.unwrap();
    let mut binding = TriggerBinding::new(ProviderId::from("github"), "webhook", "github.test");
    binding.dedupe_key = Some("event.dedupe_key".to_string());
    binding.config = json!({
        "match": { "path": "/hooks/github" },
        "secrets": { "signing_secret": "github/webhook-secret" }
    });
    connector.activate(&[binding]).await.unwrap();

    let body = br#"{"action":"opened","issue":{"number":1}}"#.to_vec();
    let signature = github_signature("topsecret", &body);
    let raw = RawInbound {
        kind: "issues".to_string(),
        headers: BTreeMap::from([
            ("X-GitHub-Event".to_string(), "issues".to_string()),
            ("X-GitHub-Delivery".to_string(), "delivery-1".to_string()),
            ("X-Hub-Signature-256".to_string(), signature),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        query: BTreeMap::new(),
        body,
        received_at: OffsetDateTime::now_utc(),
        occurred_at: None,
        tenant_id: None,
        metadata: JsonValue::Null,
    };

    let event = connector.normalize_inbound(raw.clone()).await.unwrap();
    assert_eq!(event.kind, "issues");
    assert_eq!(event.dedupe_key, "delivery-1");
    assert!(matches!(
        event.signature_status,
        crate::triggers::SignatureStatus::Verified
    ));
    match &event.provider_payload {
        crate::triggers::ProviderPayload::Known(
            crate::triggers::event::KnownProviderPayload::GitHub(
                crate::triggers::GitHubEventPayload::Issues(payload),
            ),
        ) => {
            assert_eq!(payload.common.event, "issues");
            assert_eq!(
                payload
                    .issue
                    .get("number")
                    .and_then(serde_json::Value::as_i64),
                Some(1)
            );
        }
        other => panic!("unexpected provider payload: {other:?}"),
    }

    let duplicate = connector.normalize_inbound(raw).await.unwrap_err();
    assert!(matches!(duplicate, ConnectorError::DuplicateDelivery(_)));
}

#[tokio::test]
async fn normalizes_monitor_github_webhook_events() {
    let secrets = Arc::new(StaticSecretProvider {
        namespace: "github".to_string(),
        secrets: BTreeMap::from([(
            SecretId::new("github", "webhook-secret"),
            "topsecret".to_string(),
        )]),
    });
    let mut connector = GitHubConnector::new();
    connector.init(test_ctx(secrets).await).await.unwrap();
    let mut binding = TriggerBinding::new(ProviderId::from("github"), "webhook", "github.test");
    binding.config = json!({
        "match": { "path": "/hooks/github" },
        "secrets": { "signing_secret": "github/webhook-secret" }
    });
    connector.activate(&[binding]).await.unwrap();

    let cases = [
        (
            "deployment_status",
            json!({
                "action": "created",
                "deployment_status": {"id": 11, "state": "in_progress"},
                "deployment": {"id": 21, "environment": "staging"},
            }),
        ),
        (
            "check_run",
            json!({
                "action": "completed",
                "check_run": {"id": 12, "status": "completed", "conclusion": "success"},
            }),
        ),
    ];

    for (index, (kind, payload)) in cases.iter().enumerate() {
        let body = serde_json::to_vec(payload).expect("serialize payload");
        let raw = RawInbound {
            kind: (*kind).to_string(),
            headers: BTreeMap::from([
                ("X-GitHub-Event".to_string(), (*kind).to_string()),
                (
                    "X-GitHub-Delivery".to_string(),
                    format!("delivery-monitor-{index}"),
                ),
                (
                    "X-Hub-Signature-256".to_string(),
                    github_signature("topsecret", &body),
                ),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]),
            query: BTreeMap::new(),
            body,
            received_at: OffsetDateTime::now_utc(),
            occurred_at: None,
            tenant_id: None,
            metadata: JsonValue::Null,
        };

        let event = connector.normalize_inbound(raw).await.unwrap();
        assert_eq!(event.kind, *kind);
        match (&event.provider_payload, *kind) {
            (
                crate::triggers::ProviderPayload::Known(
                    crate::triggers::event::KnownProviderPayload::GitHub(
                        crate::triggers::GitHubEventPayload::DeploymentStatus(payload),
                    ),
                ),
                "deployment_status",
            ) => {
                assert_eq!(payload.common.event, "deployment_status");
                assert_eq!(
                    payload
                        .deployment_status
                        .get("state")
                        .and_then(serde_json::Value::as_str),
                    Some("in_progress")
                );
                assert_eq!(
                    payload
                        .deployment
                        .get("environment")
                        .and_then(serde_json::Value::as_str),
                    Some("staging")
                );
            }
            (
                crate::triggers::ProviderPayload::Known(
                    crate::triggers::event::KnownProviderPayload::GitHub(
                        crate::triggers::GitHubEventPayload::CheckRun(payload),
                    ),
                ),
                "check_run",
            ) => {
                assert_eq!(payload.common.event, "check_run");
                assert_eq!(
                    payload
                        .check_run
                        .get("conclusion")
                        .and_then(serde_json::Value::as_str),
                    Some("success")
                );
            }
            other => panic!("unexpected provider payload: {other:?}"),
        }
    }
}

#[tokio::test]
async fn outbound_methods_share_cached_installation_token() {
    let scenario = Arc::new(Mutex::new(MockScenario::default()));
    let server = spawn_mock_server(8, scenario.clone());
    let base_url = server.base_url().to_string();
    let client = initialized_client(Arc::new(StaticSecretProvider {
        namespace: "github".to_string(),
        secrets: BTreeMap::new(),
    }))
    .await;

    let mut args = client_args(&base_url);
    args.as_object_mut().unwrap().insert(
        "issue_url".to_string(),
        JsonValue::String("https://github.com/octo/demo/issues/123".to_string()),
    );
    args.as_object_mut()
        .unwrap()
        .insert("body".to_string(), JsonValue::String("hello".to_string()));
    client.call("comment", args.clone()).await.unwrap();

    args.as_object_mut()
        .unwrap()
        .insert("labels".to_string(), json!(["bug", "triage"]));
    client.call("add_labels", args.clone()).await.unwrap();

    args.as_object_mut().unwrap().remove("labels");
    args.as_object_mut().unwrap().remove("issue_url");
    args.as_object_mut().unwrap().insert(
        "pr_url".to_string(),
        JsonValue::String("https://github.com/octo/demo/pull/123".to_string()),
    );
    args.as_object_mut()
        .unwrap()
        .insert("reviewers".to_string(), json!(["alice"]));
    client.call("request_review", args.clone()).await.unwrap();

    args.as_object_mut().unwrap().remove("reviewers");
    client.call("merge_pr", args.clone()).await.unwrap();

    args.as_object_mut().unwrap().remove("pr_url");
    args.as_object_mut().unwrap().insert(
        "repo".to_string(),
        JsonValue::String("octo/demo".to_string()),
    );
    args.as_object_mut()
        .unwrap()
        .insert("days".to_string(), json!(14));
    client.call("list_stale_prs", args.clone()).await.unwrap();

    args.as_object_mut().unwrap().remove("repo");
    args.as_object_mut().unwrap().remove("days");
    args.as_object_mut().unwrap().insert(
        "pr_url".to_string(),
        JsonValue::String("https://github.com/octo/demo/pull/123".to_string()),
    );
    client.call("get_pr_diff", args.clone()).await.unwrap();

    args.as_object_mut().unwrap().remove("pr_url");
    args.as_object_mut().unwrap().insert(
        "repo".to_string(),
        JsonValue::String("octo/demo".to_string()),
    );
    args.as_object_mut().unwrap().insert(
        "title".to_string(),
        JsonValue::String("new issue".to_string()),
    );
    args.as_object_mut()
        .unwrap()
        .insert("labels".to_string(), json!(["bug"]));
    client.call("create_issue", args).await.unwrap();

    drop(server);
    let state = scenario.lock().unwrap();
    assert_eq!(state.token_requests, 1);
    assert_eq!(state.api_requests.len(), 7);
    assert_eq!(state.api_requests[0].method, "POST");
    assert_eq!(
        state.api_requests[0].path,
        "/repos/octo/demo/issues/123/comments"
    );
    assert!(state.api_requests[0].body.contains("hello"));
    assert_eq!(state.api_requests[6].path, "/repos/octo/demo/issues");
}

#[tokio::test]
async fn api_call_uses_authenticated_github_rest_request() {
    let scenario = Arc::new(Mutex::new(MockScenario::default()));
    let server = spawn_mock_server(2, scenario.clone());
    let base_url = server.base_url().to_string();
    let client = initialized_client(Arc::new(StaticSecretProvider {
        namespace: "github".to_string(),
        secrets: BTreeMap::new(),
    }))
    .await;

    let response = client
        .call(
            "api_call",
            json!({
                "app_id": 123,
                "installation_id": 77,
                "api_base_url": base_url,
                "private_key_pem": TEST_PRIVATE_KEY_PEM,
                "path": "/repos/octo/demo/deployments/12/statuses",
                "method": "POST",
                "accept": "application/vnd.github.ant-man-preview+json",
                "body": {"state": "in_progress"}
            }),
        )
        .await
        .unwrap();

    drop(server);
    let state = scenario.lock().unwrap();
    assert_eq!(state.token_requests, 1);
    assert_eq!(state.api_requests.len(), 1);
    assert_eq!(state.api_requests[0].method, "POST");
    assert_eq!(
        state.api_requests[0].path,
        "/repos/octo/demo/deployments/12/statuses"
    );
    assert_eq!(
        state.api_requests[0]
            .headers
            .get("accept")
            .map(String::as_str),
        Some("application/vnd.github.ant-man-preview+json")
    );
    assert!(state.api_requests[0]
        .body
        .contains("\"state\":\"in_progress\""));
    assert_eq!(response["ok"], json!(true));
}

#[tokio::test]
async fn unauthorized_response_invalidates_token_and_remints() {
    let scenario = Arc::new(Mutex::new(MockScenario {
        unauthorized_once: HashSet::from(["/repos/octo/demo/issues/123/comments".to_string()]),
        ..MockScenario::default()
    }));
    let server = spawn_mock_server(4, scenario.clone());
    let base_url = server.base_url().to_string();
    let client = initialized_client(Arc::new(StaticSecretProvider {
        namespace: "github".to_string(),
        secrets: BTreeMap::new(),
    }))
    .await;

    let mut args = client_args(&base_url);
    let object = args.as_object_mut().unwrap();
    object.insert(
        "issue_url".to_string(),
        JsonValue::String("https://github.com/octo/demo/issues/123".to_string()),
    );
    object.insert("body".to_string(), JsonValue::String("hello".to_string()));
    client.call("comment", args).await.unwrap();

    drop(server);
    let state = scenario.lock().unwrap();
    assert_eq!(state.token_requests, 2);
    assert_eq!(state.api_requests.len(), 1);
    let first_auth = state.api_requests[0]
        .headers
        .get("authorization")
        .cloned()
        .unwrap_or_default();
    assert!(first_auth.contains("token-2"));
}

#[tokio::test]
async fn rate_limited_response_retries_once() {
    let scenario = Arc::new(Mutex::new(MockScenario {
        rate_limit_once: HashSet::from(["/repos/octo/demo/issues/123/comments".to_string()]),
        ..MockScenario::default()
    }));
    let server = spawn_mock_server(3, scenario.clone());
    let base_url = server.base_url().to_string();
    let client = initialized_client(Arc::new(StaticSecretProvider {
        namespace: "github".to_string(),
        secrets: BTreeMap::new(),
    }))
    .await;

    let mut args = client_args(&base_url);
    let object = args.as_object_mut().unwrap();
    object.insert(
        "issue_url".to_string(),
        JsonValue::String("https://github.com/octo/demo/issues/123".to_string()),
    );
    object.insert("body".to_string(), JsonValue::String("hello".to_string()));
    client.call("comment", args).await.unwrap();

    drop(server);
    let state = scenario.lock().unwrap();
    assert_eq!(state.token_requests, 1);
    assert_eq!(state.api_requests.len(), 1);
}

#[test]
fn token_store_evicts_least_recently_used_entries() {
    let store = GitHubInstallationTokenStore::new(2);
    let now = OffsetDateTime::now_utc();
    store.store(1, SecretBytes::from("one"), now + Duration::hours(1));
    store.store(2, SecretBytes::from("two"), now + Duration::hours(1));
    assert_eq!(
        store
            .get(1, now)
            .unwrap()
            .with_exposed(|bytes| String::from_utf8_lossy(bytes).to_string()),
        "one"
    );
    store.store(3, SecretBytes::from("three"), now + Duration::hours(1));
    assert!(store.get(2, now).is_none());
    assert!(store.get(1, now).is_some());
    assert!(store.get(3, now).is_some());
}
