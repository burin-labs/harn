use super::*;

use crate::event_log::{EventLog, MemoryEventLog};

fn log() -> std::sync::Arc<MemoryEventLog> {
    std::sync::Arc::new(MemoryEventLog::new(16))
}

async fn audit_events(
    log: &std::sync::Arc<MemoryEventLog>,
) -> Vec<(u64, crate::event_log::LogEvent)> {
    let topic = Topic::new(SIGNATURE_VERIFY_AUDIT_TOPIC).unwrap();
    log.read_range(&topic, None, 32).await.unwrap()
}

#[test]
fn hmac_sha256_matches_rfc4231_vectors() {
    assert_eq!(
        hex::encode(hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hex::encode(hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn hmac_sha1_matches_rfc2202_vectors() {
    assert_eq!(
        hex::encode(hmac_sha1(&[0x0b; 20], b"Hi There")),
        "b617318655057264e28bc0b6fb378c8ef146be00"
    );
    assert_eq!(
        hex::encode(hmac_sha1(
            &[0xaa; 80],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        )),
        "aa4ae5e15272d00e95705637ce8a3b55ed402112"
    );
}

#[tokio::test]
async fn verifies_github_signature_using_official_docs_vector() {
    let log = log();
    let mut headers = BTreeMap::new();
    headers.insert(
        "X-Hub-Signature-256".to_string(),
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17".to_string(),
    );

    verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("github"),
        HmacSignatureStyle::github(),
        b"Hello, World!",
        &headers,
        "It's a Secret to Everybody",
        None,
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
    )
    .await
    .unwrap();

    assert!(audit_events(&log).await.is_empty());
}

#[tokio::test]
async fn verifies_slack_signature_using_official_docs_vector() {
    let log = log();
    let headers = BTreeMap::from([
        (
            "X-Slack-Signature".to_string(),
            "v0=a2114d57b48eac39b9ad189dd8316235a7b4a8d21a10bd27519666489c69b503".to_string(),
        ),
        (
            "X-Slack-Request-Timestamp".to_string(),
            "1531420618".to_string(),
        ),
    ]);
    let body = b"token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow&channel_id=G8PSS9T3V&channel_name=foobar&user_id=U2CERLKJA&user_name=roadrunner&command=%2Fwebhook-collect&text=&response_url=https%3A%2F%2Fhooks.slack.com%2Fcommands%2FT1DC2JH3J%2F397700885554%2F96rGlfmibIGlgcZRskXaIFfN&trigger_id=398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c";

    verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("slack"),
        HmacSignatureStyle::slack(),
        body,
        &headers,
        "8f742231b10e8888abcd99yyyzzz85a5",
        Some(Duration::minutes(5)),
        OffsetDateTime::from_unix_timestamp(1_531_420_618).unwrap(),
    )
    .await
    .unwrap();

    assert!(audit_events(&log).await.is_empty());
}

#[tokio::test]
async fn verifies_standard_webhooks_using_vendor_test_vector() {
    let log = log();
    let headers = BTreeMap::from([
        (
            "webhook-id".to_string(),
            "msg_p5jXN8AQM9LWM0D4loKWxJek".to_string(),
        ),
        (
            "webhook-signature".to_string(),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=".to_string(),
        ),
        ("webhook-timestamp".to_string(), "1614265330".to_string()),
    ]);
    let now = OffsetDateTime::from_unix_timestamp(1_614_265_330).unwrap();

    verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("webhook"),
        HmacSignatureStyle::standard_webhooks(),
        br#"{"test": 2432232314}"#,
        &headers,
        "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
        Some(Duration::minutes(5)),
        now,
    )
    .await
    .unwrap();

    assert!(audit_events(&log).await.is_empty());
}

#[tokio::test]
async fn verifies_stripe_signature_using_vendor_fixture_shape() {
    let log = log();
    let headers = BTreeMap::from([(
        "Stripe-Signature".to_string(),
        "t=12345,v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e".to_string(),
    )]);
    let body = b"{\n  \"id\": \"evt_test_webhook\",\n  \"object\": \"event\"\n}";

    verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("stripe"),
        HmacSignatureStyle::stripe(),
        body,
        &headers,
        "whsec_test_secret",
        Some(Duration::seconds(30)),
        OffsetDateTime::from_unix_timestamp(12_350).unwrap(),
    )
    .await
    .unwrap();

    assert!(audit_events(&log).await.is_empty());
}

#[tokio::test]
async fn rejects_bad_signature_and_audits_failure() {
    let log = log();
    let headers = BTreeMap::from([(
        "X-Hub-Signature-256".to_string(),
        "sha256=0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    )]);

    let error = verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("github"),
        HmacSignatureStyle::github(),
        b"Hello, World!",
        &headers,
        "It's a Secret to Everybody",
        None,
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ConnectorError::InvalidSignature(_)));
    assert_eq!(audit_events(&log).await.len(), 1);
}

#[tokio::test]
async fn rejects_wrong_body_even_with_valid_github_header() {
    let log = log();
    let headers = BTreeMap::from([(
        "X-Hub-Signature-256".to_string(),
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17".to_string(),
    )]);

    let error = verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("github"),
        HmacSignatureStyle::github(),
        b"Hello, World?\n",
        &headers,
        "It's a Secret to Everybody",
        None,
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ConnectorError::InvalidSignature(_)));
    assert_eq!(audit_events(&log).await.len(), 1);
}

#[tokio::test]
async fn rejects_tampered_timestamp_header() {
    let log = log();
    let headers = BTreeMap::from([(
        "Stripe-Signature".to_string(),
        "t=not-a-timestamp,v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e"
            .to_string(),
    )]);

    let error = verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("stripe"),
        HmacSignatureStyle::stripe(),
        b"{\n  \"id\": \"evt_test_webhook\",\n  \"object\": \"event\"\n}",
        &headers,
        "whsec_test_secret",
        Some(Duration::seconds(30)),
        OffsetDateTime::from_unix_timestamp(12_350).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ConnectorError::InvalidHeader { .. }));
    assert_eq!(audit_events(&log).await.len(), 1);
}

#[tokio::test]
async fn rejects_expired_timestamp_window() {
    let log = log();
    let headers = BTreeMap::from([(
        "Stripe-Signature".to_string(),
        "t=12345,v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e".to_string(),
    )]);

    let error = verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("stripe"),
        HmacSignatureStyle::stripe(),
        b"{\n  \"id\": \"evt_test_webhook\",\n  \"object\": \"event\"\n}",
        &headers,
        "whsec_test_secret",
        Some(Duration::seconds(10)),
        OffsetDateTime::from_unix_timestamp(12_400).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ConnectorError::TimestampOutOfWindow { .. }));
    assert_eq!(audit_events(&log).await.len(), 1);
}

#[tokio::test]
async fn rejects_expired_slack_timestamp_window() {
    let log = log();
    let headers = BTreeMap::from([
        (
            "X-Slack-Signature".to_string(),
            "v0=a2114d57b48eac39b9ad189dd8316235a7b4a8d21a10bd27519666489c69b503".to_string(),
        ),
        (
            "X-Slack-Request-Timestamp".to_string(),
            "1531420618".to_string(),
        ),
    ]);
    let body = b"token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow&channel_id=G8PSS9T3V&channel_name=foobar&user_id=U2CERLKJA&user_name=roadrunner&command=%2Fwebhook-collect&text=&response_url=https%3A%2F%2Fhooks.slack.com%2Fcommands%2FT1DC2JH3J%2F397700885554%2F96rGlfmibIGlgcZRskXaIFfN&trigger_id=398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c";

    let error = verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("slack"),
        HmacSignatureStyle::slack(),
        body,
        &headers,
        "8f742231b10e8888abcd99yyyzzz85a5",
        Some(Duration::minutes(5)),
        OffsetDateTime::from_unix_timestamp(1_531_421_000).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ConnectorError::TimestampOutOfWindow { .. }));
    assert_eq!(audit_events(&log).await.len(), 1);
}

#[tokio::test]
async fn rejects_missing_signature_header() {
    let log = log();
    let headers = BTreeMap::new();

    let error = verify_hmac_signed(
        log.as_ref(),
        &ProviderId::from("github"),
        HmacSignatureStyle::github(),
        b"Hello, World!",
        &headers,
        "It's a Secret to Everybody",
        None,
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(error, ConnectorError::MissingHeader(header) if header == DEFAULT_GITHUB_SIGNATURE_HEADER)
    );
    assert_eq!(audit_events(&log).await.len(), 1);
}

fn canonical_authorization(
    secret: &str,
    method: &str,
    path: &str,
    timestamp: i64,
    body: &[u8],
) -> String {
    let signed = canonical_request_message(method, path, &timestamp.to_string(), body);
    let signature = hmac_sha256(secret.as_bytes(), signed.as_bytes());
    format!(
        "{} timestamp={},signature={}",
        DEFAULT_CANONICAL_HMAC_SCHEME,
        timestamp,
        BASE64_STANDARD.encode(signature)
    )
}

#[tokio::test]
async fn verifies_canonical_request_authorization() {
    let log = log();
    let body = br#"{"task":"review"}"#;
    let timestamp = 1_700_000_000;
    let headers = BTreeMap::from([(
        "Authorization".to_string(),
        canonical_authorization("shared-secret", "POST", "/a2a/review", timestamp, body),
    )]);

    verify_hmac_authorization(
        log.as_ref(),
        &ProviderId::from("orchestrator"),
        "POST",
        "/a2a/review",
        body,
        &headers,
        "shared-secret",
        Duration::minutes(5),
        OffsetDateTime::from_unix_timestamp(timestamp).unwrap(),
    )
    .await
    .unwrap();

    assert!(audit_events(&log).await.is_empty());
}

#[tokio::test]
async fn rejects_canonical_request_authorization_with_wrong_path() {
    let log = log();
    let body = br#"{"task":"review"}"#;
    let timestamp = 1_700_000_000;
    let headers = BTreeMap::from([(
        "authorization".to_string(),
        canonical_authorization("shared-secret", "POST", "/a2a/review", timestamp, body),
    )]);

    let error = verify_hmac_authorization(
        log.as_ref(),
        &ProviderId::from("orchestrator"),
        "POST",
        "/a2a/other",
        body,
        &headers,
        "shared-secret",
        Duration::minutes(5),
        OffsetDateTime::from_unix_timestamp(timestamp).unwrap(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ConnectorError::InvalidSignature(_)));
    assert_eq!(audit_events(&log).await.len(), 1);
}
