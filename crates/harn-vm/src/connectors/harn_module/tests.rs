use super::*;
#[path = "tests/egress_context.rs"]
mod egress_context;
use tempfile::TempDir;

use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::secrets::{
    RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
};
use crate::{InboxIndex, MetricsRegistry, RateLimiterFactory};

fn raw_inbound(body: JsonValue) -> crate::RawInbound {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    raw_inbound_with_headers(body, headers)
}

fn raw_inbound_with_headers(
    body: JsonValue,
    headers: BTreeMap<String, String>,
) -> crate::RawInbound {
    let mut raw = crate::RawInbound::new(
        "",
        headers,
        serde_json::to_vec(&body).expect("json body serializes"),
    );
    raw.received_at = OffsetDateTime::parse("2026-04-22T12:34:56Z", &Rfc3339).unwrap();
    raw
}

async fn normalize_with_harn_connector(
    source: &str,
    body: JsonValue,
    headers: BTreeMap<String, String>,
) -> TriggerEvent {
    let (_dir, module_path) = write_connector(source);
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let mut connector = HarnConnector::load(&module_path).await.unwrap();
    connector.init(ctx(log).await).await.unwrap();
    let result = connector
        .normalize_inbound_result(raw_inbound_with_headers(body, headers))
        .await
        .unwrap();
    connector.shutdown(StdDuration::ZERO).await.unwrap();
    let ConnectorNormalizeResult::Event(event) = result else {
        panic!("expected normalized event");
    };
    *event
}

fn event_value(kind: &str, dedupe_key: &str, id: &str) -> JsonValue {
    json!({
        "kind": kind,
        "occurred_at": "2026-04-22T12:30:00Z",
        "dedupe_key": dedupe_key,
        "payload": {
            "id": id,
            "type": kind,
        },
        "signature_status": {
            "state": "verified",
        },
    })
}

fn fixture_payload_schema() -> ProviderPayloadSchema {
    ProviderPayloadSchema::named("FixtureEventPayload")
}

#[test]
fn normalize_result_v1_event_parses_normal_event() {
    let provider = ProviderId::new("webhook");
    let raw = raw_inbound(json!({"id": "evt-1"}));
    let result = parse_normalize_result(
        &provider,
        &fixture_payload_schema(),
        &raw,
        json!({
            "type": "event",
            "event": event_value("webhook.received", "webhook:evt-1", "evt-1"),
        }),
    )
    .unwrap();

    let ConnectorNormalizeResult::Event(event) = result else {
        panic!("expected event result");
    };
    assert_eq!(event.provider, provider);
    assert_eq!(event.kind, "webhook.received");
    assert_eq!(event.dedupe_key, "webhook:evt-1");
    assert_eq!(event.signature_status, SignatureStatus::Verified);
    assert!(event.raw_body.is_some());
    let ProviderPayload::Extension(payload) = event.provider_payload else {
        panic!("Harn connector payload must stay package-owned");
    };
    assert_eq!(payload.provider, "webhook");
    assert_eq!(payload.schema_name, "FixtureEventPayload");
    assert_eq!(payload.raw["id"], "evt-1");
}

#[test]
fn normalize_result_v1_batch_parses_multiple_events() {
    let provider = ProviderId::new("webhook");
    let raw = raw_inbound(json!({"items": [{"id": "a"}, {"id": "b"}]}));
    let result = parse_normalize_result(
        &provider,
        &fixture_payload_schema(),
        &raw,
        json!({
            "type": "batch",
            "events": [
                event_value("webhook.received", "webhook:a", "a"),
                event_value("webhook.received", "webhook:b", "b"),
            ],
        }),
    )
    .unwrap();

    let ConnectorNormalizeResult::Batch(events) = result else {
        panic!("expected batch result");
    };
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].dedupe_key, "webhook:a");
    assert_eq!(events[1].dedupe_key, "webhook:b");
}

#[test]
fn normalize_result_v1_immediate_response_covers_slack_url_verification_fixture() {
    let provider = ProviderId::new("slack");
    let raw = raw_inbound(json!({
        "type": "url_verification",
        "challenge": "challenge-token",
    }));
    let result = parse_normalize_result(
        &provider,
        &fixture_payload_schema(),
        &raw,
        json!({
            "type": "immediate_response",
            "immediate_response": {
                "status": 200,
                "headers": {
                    "content-type": "text/plain; charset=utf-8",
                },
                "body": "challenge-token",
            },
        }),
    )
    .unwrap();

    let ConnectorNormalizeResult::ImmediateResponse { response, events } = result else {
        panic!("expected immediate_response result");
    };
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers.get("content-type").map(String::as_str),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        response.body,
        JsonValue::String("challenge-token".to_string())
    );
    assert!(events.is_empty());
}

#[test]
fn normalize_result_v1_reject_parses_http_rejection() {
    let provider = ProviderId::new("webhook");
    let raw = raw_inbound(json!({"id": "evt-1"}));
    let result = parse_normalize_result(
        &provider,
        &fixture_payload_schema(),
        &raw,
        json!({
            "type": "reject",
            "status": 403,
            "body": {
                "error": "verification_failed",
            },
        }),
    )
    .unwrap();

    let ConnectorNormalizeResult::Reject(response) = result else {
        panic!("expected reject result");
    };
    assert_eq!(response.status, 403);
    assert_eq!(response.body["error"], "verification_failed");
}

#[test]
fn direct_event_shape_is_rejected_after_v1_cutover() {
    let provider = ProviderId::new("webhook");
    let raw = raw_inbound(json!({"id": "legacy"}));
    let error = parse_normalize_result(
        &provider,
        &fixture_payload_schema(),
        &raw,
        event_value("webhook.received", "webhook:legacy", "legacy"),
    )
    .unwrap_err();

    assert!(error.to_string().contains("must return NormalizeResult v1"));
}

struct EmptySecretProvider;

#[async_trait]
impl SecretProvider for EmptySecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        Err(SecretError::NotFound {
            provider: self.namespace().to_string(),
            id: id.clone(),
        })
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

    fn namespace(&self) -> &'static str {
        "test"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

struct StaticSecretProvider;

#[async_trait]
impl SecretProvider for StaticSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        if id.to_string().starts_with("test/signing-secret") {
            return Ok(SecretBytes::from("local-secret"));
        }
        Err(SecretError::NotFound {
            provider: self.namespace().to_string(),
            id: id.clone(),
        })
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

    fn namespace(&self) -> &'static str {
        "test"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

async fn ctx(log: Arc<AnyEventLog>) -> ConnectorCtx {
    let metrics = Arc::new(MetricsRegistry::default());
    ConnectorCtx {
        inbox: Arc::new(InboxIndex::new(log.clone(), metrics.clone()).await.unwrap()),
        event_log: log,
        secrets: Arc::new(EmptySecretProvider),
        metrics,
        rate_limiter: Arc::new(RateLimiterFactory::default()),
    }
}

async fn ctx_with_secrets(log: Arc<AnyEventLog>, secrets: Arc<dyn SecretProvider>) -> ConnectorCtx {
    let metrics = Arc::new(MetricsRegistry::default());
    ConnectorCtx {
        inbox: Arc::new(InboxIndex::new(log.clone(), metrics.clone()).await.unwrap()),
        event_log: log,
        secrets,
        metrics,
        rate_limiter: Arc::new(RateLimiterFactory::default()),
    }
}

fn write_connector(source: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connector.harn");
    std::fs::write(&path, source).unwrap();
    (dir, path)
}

/// Regression test for the dependency-package source-dir leak: loading a
/// connector contract (as `harn run` does for every installed
/// `[dependencies]` provider before executing the entry pipeline) spins up
/// an isolated `Vm` and calls `set_source_dir` on the connector's own dir,
/// which writes the *shared* thread-local `VM_SOURCE_DIR`. Before the fix,
/// that write was never restored, so the caller's resting source-dir
/// context — the anchor for top-level `render("@alias/...")` and
/// source-relative asset resolution — was left pointing at the dependency
/// package instead of the project root. The load must leave the caller's
/// thread-local source dir exactly as it found it.
#[tokio::test]
async fn load_contract_does_not_leak_thread_source_dir() {
    // Stand in for the entry project: a dir whose `harn.toml` the caller's
    // top-level asset resolution should keep anchoring on.
    let project_dir = tempfile::tempdir().unwrap();
    crate::stdlib::process::reset_process_state();
    crate::stdlib::set_thread_source_dir(project_dir.path());
    let before = crate::stdlib::process::source_root_path();

    // The connector module lives in a *different* dir, like a materialized
    // dependency package generation.
    let (_dir, module_path) = write_connector(
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
"#,
    );
    let _contract = load_contract(&module_path).await.unwrap();

    let after = crate::stdlib::process::source_root_path();
    assert_eq!(
        after, before,
        "loading a connector contract must not leak the thread-local source \
         dir (before={before:?}, after={after:?})"
    );
    crate::stdlib::process::reset_process_state();
}

#[tokio::test]
async fn load_contract_reads_exact_connector_method_inventory() {
    let (_dir, module_path) = write_connector(
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn methods() {
  return [
    {name: "messages.read"},
    {name: "messages.send", requires_approval: true},
  ]
}
"#,
    );

    let contract = load_contract(&module_path).await.unwrap();

    assert_eq!(
        contract.method_ids,
        Some(vec![
            "messages.read".to_string(),
            "messages.send".to_string()
        ])
    );
}

#[tokio::test]
async fn runtime_exports_require_a_typed_leading_harness_at_load_time() {
    let (_dir, module_path) = write_connector(
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(raw) {
  return {type: "reject", status: 400, body: raw}
}
"#,
    );
    let error = load_contract(&module_path)
        .await
        .expect_err("ambient connector entrypoint must fail closed");
    let message = error.to_string();
    assert!(
        message.contains(
            "runtime export 'normalize_inbound' must declare `harness: Harness` as its first parameter"
        ),
        "{message}"
    );
}

#[tokio::test]
async fn normalize_inbound_default_policy_allows_local_hot_path_work() {
    let (_dir, module_path) = write_connector(
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }

pub fn normalize_inbound(harness: Harness, raw) {
  const decoded = base64_decode(raw.body_base64)
  const body = json_parse(decoded)
  const secret = harness.secrets.read("test/signing-secret")
  const signature = hmac_sha256(secret, decoded)
  harness.obs.metrics_inc("normalize_ok")
  return {
type: "event",
event: {
  kind: "webhook.received",
  dedupe_key: "webhook:" + body.id,
  payload: {id: body.id, signature: signature},
  signature_status: {state: "verified"},
},
  }
}
"#,
    );
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let mut connector = HarnConnector::load(&module_path).await.unwrap();
    connector
        .init(ctx_with_secrets(log, Arc::new(StaticSecretProvider)).await)
        .await
        .unwrap();
    let result = connector
        .normalize_inbound_result(raw_inbound(json!({"id": "evt-1"})))
        .await
        .unwrap();
    connector.shutdown(StdDuration::ZERO).await.unwrap();

    let ConnectorNormalizeResult::Event(event) = result else {
        panic!("expected normalized event");
    };
    assert_eq!(event.kind, "webhook.received");
    assert_eq!(event.signature_status, SignatureStatus::Verified);
    let ProviderPayload::Extension(payload) = &event.provider_payload else {
        panic!("Harn connector payload must remain package-owned");
    };
    assert_eq!(payload.raw["id"], "evt-1");
}

#[tokio::test]
async fn pure_harn_connectors_preserve_package_owned_payload_shapes() {
    let github_event = normalize_with_harn_connector(
        r#"
pub fn provider_id() { return "github" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GitHubEventPayload" }

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json
  return {
type: "event",
event: {
  kind: raw.headers["X-GitHub-Event"],
  dedupe_key: raw.headers["X-GitHub-Delivery"],
  payload: body,
  signature_status: {state: "verified"},
},
  }
}
"#,
        json!({
            "action": "opened",
            "installation": {"id": 101},
            "issue": {"number": 42, "title": "Contract drift"}
        }),
        BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-GitHub-Event".to_string(), "issues".to_string()),
            ("X-GitHub-Delivery".to_string(), "delivery-gh-1".to_string()),
        ]),
    )
    .await;
    assert_eq!(github_event.kind, "issues");
    assert_eq!(github_event.dedupe_key, "delivery-gh-1");
    assert_eq!(github_event.signature_status, SignatureStatus::Verified);
    assert_extension_payload(
        &github_event,
        "github",
        "GitHubEventPayload",
        "issue",
        json!({"number": 42, "title": "Contract drift"}),
    );

    let slack_event = normalize_with_harn_connector(
        r#"
pub fn provider_id() { return "slack" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "SlackEventPayload" }

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json
  return {
type: "event",
event: {
  kind: body.event.type + "." + body.event.channel_type,
  dedupe_key: "slack:" + body.event_id,
  payload: body,
  signature_status: {state: "verified"},
},
  }
}
"#,
        json!({
            "team_id": "T123ABC456",
            "api_app_id": "A123ABC456",
            "type": "event_callback",
            "event_id": "Ev123MESSAGE",
            "event": {
                "type": "message",
                "user": "U123ABC456",
                "text": "hello from a channel",
                "ts": "1715000000.000100",
                "channel": "C123ABC456",
                "channel_type": "channel",
                "event_ts": "1715000000.000100"
            }
        }),
        BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]),
    )
    .await;
    assert_eq!(slack_event.kind, "message.channel");
    assert_extension_payload(
        &slack_event,
        "slack",
        "SlackEventPayload",
        "event",
        json!({
            "type": "message",
            "user": "U123ABC456",
            "text": "hello from a channel",
            "ts": "1715000000.000100",
            "channel": "C123ABC456",
            "channel_type": "channel",
            "event_ts": "1715000000.000100"
        }),
    );

    let linear_event = normalize_with_harn_connector(
        r#"
pub fn provider_id() { return "linear" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "LinearEventPayload" }

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json
  return {
type: "event",
event: {
  kind: "issue." + body.action,
  dedupe_key: raw.headers["Linear-Delivery"],
  payload: body,
  signature_status: {state: "verified"},
},
  }
}
"#,
        json!({
            "action": "update",
            "type": "Issue",
            "organizationId": "org_123",
            "webhookTimestamp": 1715000000000i64,
            "webhookId": "wh_123",
            "actor": {"id": "user_1", "name": "Ada"},
            "data": {"id": "ISS-1", "title": "Fix Linear connector"},
            "updatedFrom": {"title": "Previous title", "labelIds": ["lbl_1"]}
        }),
        BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Linear-Delivery".to_string(),
                "delivery-linear-1".to_string(),
            ),
        ]),
    )
    .await;
    assert_eq!(linear_event.kind, "issue.update");
    assert_extension_payload(
        &linear_event,
        "linear",
        "LinearEventPayload",
        "data",
        json!({"id": "ISS-1", "title": "Fix Linear connector"}),
    );

    let notion_event = normalize_with_harn_connector(
        r#"
pub fn provider_id() { return "notion" }
pub fn kinds() { return ["webhook", "poll"] }
pub fn payload_schema() { return "NotionEventPayload" }

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json
  return {
type: "event",
event: {
  kind: body.type,
  dedupe_key: "notion:" + body.entity.id,
  payload: body,
  signature_status: {state: "verified"},
},
  }
}
"#,
        json!({
            "id": "evt_1",
            "type": "page.content_updated",
            "workspace_id": "ws_1",
            "subscription_id": "sub_1",
            "integration_id": "int_1",
            "entity": {"id": "page_1", "type": "page"},
            "api_version": "2022-06-28"
        }),
        BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            ("request-id".to_string(), "req_123".to_string()),
        ]),
    )
    .await;
    assert_eq!(notion_event.kind, "page.content_updated");
    assert_extension_payload(
        &notion_event,
        "notion",
        "NotionEventPayload",
        "entity",
        json!({"id": "page_1", "type": "page"}),
    );
}

fn assert_extension_payload(
    event: &TriggerEvent,
    provider: &str,
    schema_name: &str,
    field: &str,
    expected: JsonValue,
) {
    let ProviderPayload::Extension(payload) = &event.provider_payload else {
        panic!("connector payload must remain package-owned: {event:?}");
    };
    assert_eq!(payload.provider, provider);
    assert_eq!(payload.schema_name, schema_name);
    assert_eq!(payload.raw[field], expected);
}

#[tokio::test]
async fn normalize_inbound_default_policy_denies_network_llm_and_file_effects() {
    for (label, source, expected) in [
        (
            "network",
            r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(harness: Harness, _raw) {
  harness.net.get("https://example.invalid")
  return {type: "reject", status: 400}
}
"#,
            "net:read",
        ),
        (
            "llm",
            r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(harness: Harness, _raw) {
  harness.llm.call("hello", nil, {provider: "mock"})
  return {type: "reject", status: 400}
}
"#,
            "llm:mock:write",
        ),
        (
            "file",
            r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(harness: Harness, _raw) {
  harness.fs.read_text("ambient.txt")
  return {type: "reject", status: 400}
}
"#,
            "fs:read",
        ),
    ] {
        let (_dir, module_path) = write_connector(source);
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let mut connector = HarnConnector::load(&module_path).await.unwrap();
        connector.init(ctx(log).await).await.unwrap();
        let error = connector
            .normalize_inbound_result(raw_inbound(json!({"id": label})))
            .await
            .unwrap_err();
        connector.shutdown(StdDuration::ZERO).await.unwrap();
        let message = error.to_string();
        assert!(
            message.contains("connector export 'normalize_inbound' violated effect policy"),
            "{label}: {message}"
        );
        assert!(message.contains(expected), "{label}: {message}");
    }
}

fn write_poll_connector() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poll_connector.harn");
    std::fs::write(
        &path,
        r#"
pub fn provider_id() {
  return "webhook"
}

pub fn kinds() {
  return ["poll"]
}

pub fn payload_schema() {
  return "GenericWebhookPayload"
}

pub fn poll_tick(_harness: Harness, ctx) {
  let previous = 0
  if ctx.cursor != nil && ctx.cursor.count != nil {
previous = ctx.cursor.count
  }
  const next = previous + 1
  return {
cursor: {count: next},
state: {last_lease_id: ctx.lease.id, tenant_id: ctx.tenant_id},
events: [
  {
    kind: "webhook.poll",
    dedupe_key: "poll-" + to_string(next),
    payload: {
      count: next,
      previous: previous,
      max_batch_size: ctx.max_batch_size,
      tenant_id: ctx.tenant_id,
      lease_id: ctx.lease.id,
    },
  },
],
  }
}
"#,
    )
    .unwrap();
    (dir, path)
}

async fn read_topic(log: &Arc<AnyEventLog>, topic: &str) -> Vec<(u64, crate::event_log::LogEvent)> {
    let topic = Topic::new(topic).unwrap();
    log.read_range(&topic, None, usize::MAX).await.unwrap()
}

#[tokio::test]
async fn poll_tick_emits_inbox_events_and_persists_cursor_state() {
    let _clock = clock::install_override(clock::MockClock::new(
        OffsetDateTime::parse("2026-04-22T12:34:56Z", &Rfc3339).unwrap(),
    ));
    let (_dir, module_path) = write_poll_connector();
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(128)));
    let connector_ctx = ctx(log.clone()).await;
    let mut connector = HarnConnector::load(&module_path).await.unwrap();
    connector.init(connector_ctx.clone()).await.unwrap();

    let mut binding = TriggerBinding::new(ProviderId::from("webhook"), "poll", "poll-source");
    binding.dedupe_key = Some("event.dedupe_key".to_string());
    binding.config = json!({
        "poll": {
            "interval_ms": 1000,
            "state_key": "tenant-a-source",
            "lease_id": "lease-a",
            "tenant_id": "tenant-a",
            "max_batch_size": 1,
        }
    });

    let resolved = resolve_poll_binding(&binding).unwrap();
    let worker = connector.shared.worker().unwrap();
    let connector_ctx = connector.shared.ctx().unwrap();
    let shutdown = Arc::new(PollShutdownSignal::default());
    run_poll_tick(
        &connector.provider_id,
        &connector.payload_schema,
        worker.clone(),
        &connector_ctx,
        &resolved,
        shutdown.clone(),
    )
    .await
    .unwrap();
    clock::advance(StdDuration::from_secs(1));
    run_poll_tick(
        &connector.provider_id,
        &connector.payload_schema,
        worker,
        &connector_ctx,
        &resolved,
        shutdown,
    )
    .await
    .unwrap();
    connector.shutdown(StdDuration::ZERO).await.unwrap();

    let inbox = read_topic(&log, crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC).await;
    let envelopes = inbox
        .into_iter()
        .map(|(_, event)| serde_json::from_value::<InboxEnvelope>(event.payload).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(envelopes[0].trigger_id.as_deref(), Some("poll-source"));
    assert_eq!(envelopes[0].event.dedupe_key, "poll-1");
    assert_eq!(envelopes[1].event.dedupe_key, "poll-2");
    assert_eq!(
        envelopes[1]
            .event
            .tenant_id
            .as_ref()
            .map(|tenant| tenant.0.as_str()),
        Some("tenant-a")
    );
    let ProviderPayload::Extension(payload) = &envelopes[1].event.provider_payload else {
        panic!("poll payload must remain package-owned");
    };
    assert_eq!(payload.raw["previous"], 1);
    assert_eq!(payload.raw["max_batch_size"], 1);
    assert_eq!(payload.raw["tenant_id"], "tenant-a");
    assert_eq!(payload.raw["lease_id"], "lease-a");

    let observed = read_topic(&log, crate::triggers::TRIGGER_INBOX_OBSERVABILITY_TOPIC).await;
    assert_eq!(observed.len(), 2);
    assert_eq!(
        observed[0].1.payload["trigger_id"],
        serde_json::json!("poll-source")
    );
    assert_eq!(
        observed[1].1.payload["event"]["tenant_id"],
        serde_json::json!("tenant-a")
    );
    assert!(observed[1].1.payload["event"]
        .get("provider_payload")
        .is_none());

    let states = read_topic(&log, HARN_CONNECTOR_POLL_STATE_TOPIC).await;
    assert_eq!(states.len(), 2);
    let latest: HarnPollStateRecord =
        serde_json::from_value(states.last().unwrap().1.payload.clone()).unwrap();
    assert_eq!(latest.provider, "webhook");
    assert_eq!(latest.binding_id, "poll-source");
    assert_eq!(latest.state_key, "tenant-a-source");
    assert_eq!(latest.cursor.unwrap()["count"], 2);
    assert_eq!(latest.state.unwrap()["last_lease_id"], "lease-a");
}

#[tokio::test]
async fn poll_binding_requires_poll_tick_export() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing_poll.harn");
    std::fs::write(
        &path,
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["poll"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
"#,
    )
    .unwrap();
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let mut connector = HarnConnector::load(&path).await.unwrap();
    connector.init(ctx(log).await).await.unwrap();
    let binding = TriggerBinding::new(ProviderId::from("webhook"), "poll", "poll-source");

    let error = connector.activate(&[binding]).await.unwrap_err();
    assert!(
        error.to_string().contains("does not export poll_tick(ctx)"),
        "{error}"
    );
    connector.shutdown(StdDuration::from_secs(1)).await.unwrap();
}
