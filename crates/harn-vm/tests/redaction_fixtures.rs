#![recursion_limit = "256"]
//! End-to-end fixture coverage for [`harn_vm::redact`].
//!
//! These tests stage representative secrets — Stripe keys, GitHub
//! PATs, AWS access key ids, Bearer tokens, auth-shaped JSON fields,
//! URLs with credentials in query and userinfo — through every
//! persistence surface that the unified policy is supposed to cover:
//! receipts, event-log entries, workflow artifacts, and the portal
//! transcript step projection. A regression in any one of those write
//! paths would silently leak the same secret through the matching
//! surface, so the assertions below double as a contract for hosts.

use std::collections::BTreeMap;

use harn_vm::event_log::LogEvent;
use harn_vm::orchestration::ArtifactRecord;
use harn_vm::receipts::{Receipt, ReceiptSink, ReceiptStatus};
use harn_vm::redact::{
    current_policy, push_policy, RedactionPolicy, REDACTED_HEADER_VALUE, REDACTED_PLACEHOLDER,
};
use serde_json::{json, Value as JsonValue};
use time::OffsetDateTime;

// Fake secrets are concatenated at runtime so the source file does not
// itself contain a string that GitHub push protection or downstream
// secret scanners would flag as a real Stripe / GitHub / AWS key. Each
// fragment still produces a payload that matches the redactor's
// regexes.
fn aws_fixture() -> String {
    format!("AKIA{}", "ABCDEFGHIJKLMNOP")
}

fn github_pat_fixture() -> String {
    format!("ghp_{}", "a".repeat(36))
}

fn openai_fixture() -> String {
    format!("sk-{}", "abcdefghijklmnopqrstuvwxyz0123456789ABCD")
}

fn stripe_fixture() -> String {
    let head = ["sk", "live"].join("_");
    format!("{head}_{}", "abcdefghijklmnopqrstuvwxyz")
}

fn bearer_fixture() -> String {
    format!("Bearer {}", "abcDEF123_-+/=longenoughtoken")
}

fn url_with_credentials_fixture() -> String {
    "https://user:pw@api.example.com/v1?api_key=hideme".to_string()
}

fn fixture_secrets() -> Vec<String> {
    vec![
        aws_fixture(),
        github_pat_fixture(),
        openai_fixture(),
        stripe_fixture(),
        bearer_fixture(),
        url_with_credentials_fixture(),
    ]
}

fn assert_redacted(rendered: &str) {
    for secret in fixture_secrets() {
        assert!(
            !rendered.contains(&secret),
            "secret `{secret}` leaked into rendered output:\n{rendered}"
        );
    }
    assert!(
        rendered.contains(REDACTED_PLACEHOLDER) || rendered.contains(REDACTED_HEADER_VALUE),
        "expected redacted placeholder somewhere in rendered output:\n{rendered}"
    );
}

fn fixture_receipt() -> Receipt {
    let mut receipt = Receipt::new(
        "receipt_01JZFIXTURE",
        "merge_captain",
        "trace_01JZFIXTURE",
        OffsetDateTime::from_unix_timestamp(1_777_000_000).unwrap(),
    )
    .completed(
        OffsetDateTime::from_unix_timestamp(1_777_000_030).unwrap(),
        ReceiptStatus::Success,
    );
    let model_call = BTreeMap::from([
        ("step".to_string(), JsonValue::String("classify".into())),
        (
            "request_url".to_string(),
            JsonValue::String(format!("{}&page=2", url_with_credentials_fixture())),
        ),
        (
            "authorization".to_string(),
            JsonValue::String(bearer_fixture()),
        ),
        (
            "raw_response".to_string(),
            JsonValue::String(format!("token leak: {}", github_pat_fixture())),
        ),
    ]);
    receipt.model_calls.push(model_call);
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "stripe_secret".to_string(),
        JsonValue::String(stripe_fixture()),
    );
    metadata.insert(
        "audit_note".to_string(),
        JsonValue::String(format!("found {} in env", aws_fixture())),
    );
    receipt.metadata = metadata;
    receipt
}

#[test]
fn receipt_redact_in_place_scrubs_every_known_secret_shape() {
    harn_vm::reset_thread_local_state();
    let mut receipt = fixture_receipt();
    receipt.redact_in_place(&RedactionPolicy::default());

    let json = serde_json::to_string(&receipt).expect("receipt serializes");
    assert_redacted(&json);

    // Envelope identity fields must remain stable so receipt
    // reconciliation, replay oracles, and trust-graph joins still
    // function on redacted persisted data.
    assert!(json.contains("receipt_01JZFIXTURE"));
    assert!(json.contains("trace_01JZFIXTURE"));
    assert!(json.contains("merge_captain"));
}

#[test]
fn redacting_receipt_sink_wraps_inner_persistence() {
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Captured(Arc<Mutex<Option<Receipt>>>);
    #[async_trait]
    impl harn_vm::receipts::ReceiptSink for Captured {
        type Error = std::convert::Infallible;
        async fn persist_receipt(&self, receipt: &Receipt) -> Result<(), Self::Error> {
            *self.0.lock().unwrap() = Some(receipt.clone());
            Ok(())
        }
    }

    let captured = Captured(Arc::new(Mutex::new(None)));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let sink = harn_vm::receipts::RedactingReceiptSink::new(
            captured.clone(),
            RedactionPolicy::default(),
        );
        sink.persist_receipt(&fixture_receipt()).await.unwrap();
    });
    let stored = captured
        .0
        .lock()
        .unwrap()
        .take()
        .expect("sink received a receipt");
    let json = serde_json::to_string(&stored).unwrap();
    assert_redacted(&json);
}

#[test]
fn log_event_redact_in_place_scrubs_payload_and_headers() {
    let mut event = LogEvent::new(
        "stdlib.git.receipt",
        json!({
            "tool_calls": [
                {
                    "headers": {
                        "Authorization": bearer_fixture(),
                        "X-Webhook-Token": "tok_should_redact",
                        "User-Agent": "Harn/1.0"
                    },
                    "url": url_with_credentials_fixture(),
                    "stdout": format!("credential leak: {}", aws_fixture()),
                }
            ]
        }),
    )
    .with_headers(BTreeMap::from([
        ("Authorization".to_string(), bearer_fixture()),
        ("X-Request-Id".to_string(), "req-123".to_string()),
    ]));

    event.redact_in_place(&RedactionPolicy::default());

    let json = serde_json::to_string(&event).unwrap();
    assert_redacted(&json);
    assert_eq!(event.headers.get("X-Request-Id").unwrap(), "req-123");
}

#[test]
fn artifact_record_redact_in_place_scrubs_text_and_metadata() {
    let mut artifact = ArtifactRecord {
        type_name: "artifact".to_string(),
        id: "artifact_01J".to_string(),
        kind: "tool_output".to_string(),
        title: Some("git diff".to_string()),
        text: Some(format!(
            "tested with {} and {}",
            stripe_fixture(),
            github_pat_fixture()
        )),
        data: Some(json!({
            "request": { "Authorization": bearer_fixture() }
        })),
        source: None,
        created_at: "2026-05-09T00:00:00Z".to_string(),
        freshness: None,
        priority: None,
        lineage: Vec::new(),
        relevance: None,
        estimated_tokens: None,
        stage: None,
        metadata: BTreeMap::from([
            ("api_key".to_string(), JsonValue::String(aws_fixture())),
            ("note".to_string(), JsonValue::String("scope: read".into())),
        ]),
    };

    artifact.redact_in_place(&RedactionPolicy::default());

    let json = serde_json::to_string(&artifact).unwrap();
    assert_redacted(&json);
    assert!(json.contains("git diff"));
    assert!(json.contains("scope: read"));
}

#[test]
fn current_policy_falls_back_to_default_when_stack_empty() {
    harn_vm::reset_thread_local_state();
    let policy = current_policy();
    assert_eq!(policy, RedactionPolicy::default());
}

#[test]
fn host_can_override_policy_via_thread_local_stack() {
    harn_vm::reset_thread_local_state();
    push_policy(
        RedactionPolicy::default()
            .with_extra_field("internal_audit_token")
            .with_safe_header("X-Webhook-Token"),
    );

    let policy = current_policy();
    assert!(policy.field_is_sensitive("internal_audit_token"));
    let mut headers = BTreeMap::new();
    headers.insert("X-Webhook-Token".to_string(), "tok_keep_me".to_string());
    headers.insert("X-Other-Token".to_string(), "tok_redact_me".to_string());
    let redacted = policy.redact_headers(&headers);
    assert_eq!(redacted.get("X-Webhook-Token").unwrap(), "tok_keep_me");
    assert_eq!(
        redacted.get("X-Other-Token").unwrap(),
        REDACTED_HEADER_VALUE
    );

    harn_vm::reset_thread_local_state();
    assert_eq!(current_policy(), RedactionPolicy::default());
}
