use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};

use crate::event_log::{EventLog, LogEvent, Topic};
use crate::triggers::ProviderId;

use super::ConnectorError;

pub const SIGNATURE_VERIFY_AUDIT_TOPIC: &str = "audit.signature_verify";
pub const DEFAULT_GITHUB_SIGNATURE_HEADER: &str = "x-hub-signature-256";
pub const DEFAULT_STRIPE_SIGNATURE_HEADER: &str = "stripe-signature";
pub const DEFAULT_STANDARD_WEBHOOKS_ID_HEADER: &str = "webhook-id";
pub const DEFAULT_STANDARD_WEBHOOKS_SIGNATURE_HEADER: &str = "webhook-signature";
pub const DEFAULT_STANDARD_WEBHOOKS_TIMESTAMP_HEADER: &str = "webhook-timestamp";

/// Supported HMAC signature header conventions for inbound webhook providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HmacSignatureStyle<'a> {
    GitHub {
        signature_header: &'a str,
        prefix: &'a str,
    },
    Stripe {
        signature_header: &'a str,
        version: &'a str,
    },
    StandardWebhooks {
        id_header: &'a str,
        signature_header: &'a str,
        timestamp_header: &'a str,
        version: &'a str,
    },
}

impl<'a> HmacSignatureStyle<'a> {
    pub fn github() -> Self {
        Self::GitHub {
            signature_header: DEFAULT_GITHUB_SIGNATURE_HEADER,
            prefix: "sha256=",
        }
    }

    pub fn stripe() -> Self {
        Self::Stripe {
            signature_header: DEFAULT_STRIPE_SIGNATURE_HEADER,
            version: "v1",
        }
    }

    pub fn standard_webhooks() -> Self {
        Self::StandardWebhooks {
            id_header: DEFAULT_STANDARD_WEBHOOKS_ID_HEADER,
            signature_header: DEFAULT_STANDARD_WEBHOOKS_SIGNATURE_HEADER,
            timestamp_header: DEFAULT_STANDARD_WEBHOOKS_TIMESTAMP_HEADER,
            version: "v1",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::GitHub { .. } => "github",
            Self::Stripe { .. } => "stripe",
            Self::StandardWebhooks { .. } => "standard_webhooks",
        }
    }
}

/// Verify an HMAC-signed raw request body against one of the supported webhook
/// header conventions.
pub async fn verify_hmac_signed<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    body: &[u8],
    headers: &BTreeMap<String, String>,
    secret: &str,
    timestamp_window: Option<Duration>,
    now: OffsetDateTime,
) -> Result<(), ConnectorError> {
    match style {
        HmacSignatureStyle::GitHub {
            signature_header,
            prefix,
        } => {
            verify_github(
                event_log,
                provider,
                style,
                signature_header,
                prefix,
                body,
                headers,
                secret,
                now,
            )
            .await
        }
        HmacSignatureStyle::Stripe {
            signature_header,
            version,
        } => {
            verify_stripe(
                event_log,
                provider,
                style,
                signature_header,
                version,
                body,
                headers,
                secret,
                timestamp_window,
                now,
            )
            .await
        }
        HmacSignatureStyle::StandardWebhooks {
            id_header,
            signature_header,
            timestamp_header,
            version,
        } => {
            verify_standard_webhooks(
                event_log,
                provider,
                style,
                id_header,
                signature_header,
                timestamp_header,
                version,
                body,
                headers,
                secret,
                timestamp_window,
                now,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn verify_github<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    signature_header: &str,
    prefix: &str,
    body: &[u8],
    headers: &BTreeMap<String, String>,
    secret: &str,
    now: OffsetDateTime,
) -> Result<(), ConnectorError> {
    let header = required_header(headers, signature_header).map_err(|error| {
        ConnectorError::MissingHeader(match error {
            ConnectorError::MissingHeader(name) => name,
            other => other.to_string(),
        })
    });
    let header = match header {
        Ok(value) => value,
        Err(error) => return reject(event_log, provider, style, &error, now, None, None).await,
    };

    let encoded = match header.strip_prefix(prefix) {
        Some(value) => value,
        None => {
            let error = ConnectorError::InvalidHeader {
                name: signature_header.to_string(),
                detail: format!("expected `{prefix}` prefix"),
            };
            return reject(event_log, provider, style, &error, now, None, None).await;
        }
    };
    let provided = match hex::decode(encoded) {
        Ok(value) => value,
        Err(error) => {
            let error = ConnectorError::InvalidHeader {
                name: signature_header.to_string(),
                detail: error.to_string(),
            };
            return reject(event_log, provider, style, &error, now, None, None).await;
        }
    };

    let expected = hmac_sha256(secret.as_bytes(), body);
    if secure_eq(&expected, &provided) {
        Ok(())
    } else {
        let error =
            ConnectorError::invalid_signature("signature did not match the raw request body");
        reject(event_log, provider, style, &error, now, None, None).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn verify_stripe<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    signature_header: &str,
    version: &str,
    body: &[u8],
    headers: &BTreeMap<String, String>,
    secret: &str,
    timestamp_window: Option<Duration>,
    now: OffsetDateTime,
) -> Result<(), ConnectorError> {
    let window = match timestamp_window {
        Some(window) => window,
        None => {
            let error = ConnectorError::Unsupported(
                "stripe-style signature verification requires a timestamp window".to_string(),
            );
            return reject(event_log, provider, style, &error, now, None, None).await;
        }
    };

    let header = match required_header(headers, signature_header) {
        Ok(value) => value,
        Err(error) => {
            return reject(event_log, provider, style, &error, now, None, Some(window)).await
        }
    };

    let mut timestamp = None;
    let mut provided = Vec::new();
    for part in header.split(',') {
        let (key, value) = match part.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        if key == "t" {
            match value.parse::<i64>() {
                Ok(raw) => match OffsetDateTime::from_unix_timestamp(raw) {
                    Ok(parsed) => timestamp = Some(parsed),
                    Err(error) => {
                        let error = ConnectorError::InvalidHeader {
                            name: signature_header.to_string(),
                            detail: error.to_string(),
                        };
                        return reject(event_log, provider, style, &error, now, None, Some(window))
                            .await;
                    }
                },
                Err(error) => {
                    let error = ConnectorError::InvalidHeader {
                        name: signature_header.to_string(),
                        detail: error.to_string(),
                    };
                    return reject(event_log, provider, style, &error, now, None, Some(window))
                        .await;
                }
            }
        } else if key == version {
            match hex::decode(value) {
                Ok(signature) => provided.push(signature),
                Err(error) => {
                    let error = ConnectorError::InvalidHeader {
                        name: signature_header.to_string(),
                        detail: error.to_string(),
                    };
                    return reject(event_log, provider, style, &error, now, None, Some(window))
                        .await;
                }
            }
        }
    }

    let timestamp = match timestamp {
        Some(value) => value,
        None => {
            let error = ConnectorError::InvalidHeader {
                name: signature_header.to_string(),
                detail: "missing `t=` timestamp component".to_string(),
            };
            return reject(event_log, provider, style, &error, now, None, Some(window)).await;
        }
    };
    ensure_timestamp_within_window(event_log, provider, style, timestamp, window, now).await?;

    if provided.is_empty() {
        let error = ConnectorError::InvalidHeader {
            name: signature_header.to_string(),
            detail: format!("missing `{version}=` signature component"),
        };
        return reject(
            event_log,
            provider,
            style,
            &error,
            now,
            Some(timestamp),
            Some(window),
        )
        .await;
    }

    let mut signed = timestamp.unix_timestamp().to_string().into_bytes();
    signed.push(b'.');
    signed.extend_from_slice(body);
    let expected = hmac_sha256(secret.as_bytes(), &signed);
    if provided
        .iter()
        .any(|signature| secure_eq(&expected, signature))
    {
        Ok(())
    } else {
        let error =
            ConnectorError::invalid_signature("no stripe signature matched the raw request body");
        reject(
            event_log,
            provider,
            style,
            &error,
            now,
            Some(timestamp),
            Some(window),
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn verify_standard_webhooks<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    id_header: &str,
    signature_header: &str,
    timestamp_header: &str,
    version: &str,
    body: &[u8],
    headers: &BTreeMap<String, String>,
    secret: &str,
    timestamp_window: Option<Duration>,
    now: OffsetDateTime,
) -> Result<(), ConnectorError> {
    let window = match timestamp_window {
        Some(window) => window,
        None => {
            let error = ConnectorError::Unsupported(
                "standard-webhooks verification requires a timestamp window".to_string(),
            );
            return reject(event_log, provider, style, &error, now, None, None).await;
        }
    };

    let message_id = match required_header(headers, id_header) {
        Ok(value) => value,
        Err(error) => {
            return reject(event_log, provider, style, &error, now, None, Some(window)).await
        }
    };
    let signature_header_value = match required_header(headers, signature_header) {
        Ok(value) => value,
        Err(error) => {
            return reject(event_log, provider, style, &error, now, None, Some(window)).await
        }
    };
    let timestamp_raw = match required_header(headers, timestamp_header) {
        Ok(value) => value,
        Err(error) => {
            return reject(event_log, provider, style, &error, now, None, Some(window)).await
        }
    };

    let timestamp = match timestamp_raw.parse::<i64>() {
        Ok(raw) => match OffsetDateTime::from_unix_timestamp(raw) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                let error = ConnectorError::InvalidHeader {
                    name: timestamp_header.to_string(),
                    detail: error.to_string(),
                };
                return reject(event_log, provider, style, &error, now, None, Some(window)).await;
            }
        },
        Err(error) => {
            let error = ConnectorError::InvalidHeader {
                name: timestamp_header.to_string(),
                detail: error.to_string(),
            };
            return reject(event_log, provider, style, &error, now, None, Some(window)).await;
        }
    };
    ensure_timestamp_within_window(event_log, provider, style, timestamp, window, now).await?;

    let signing_key = match decode_standard_webhooks_secret(secret) {
        Ok(secret) => secret,
        Err(error) => {
            return reject(
                event_log,
                provider,
                style,
                &error,
                now,
                Some(timestamp),
                Some(window),
            )
            .await
        }
    };

    let mut signed = message_id.as_bytes().to_vec();
    signed.push(b'.');
    signed.extend_from_slice(timestamp_raw.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);
    let expected = hmac_sha256(&signing_key, &signed);

    let mut any_v1 = false;
    for versioned in signature_header_value.split_ascii_whitespace() {
        let Some((current_version, encoded_signature)) = versioned.split_once(',') else {
            continue;
        };
        if current_version != version {
            continue;
        }
        any_v1 = true;
        let provided = match BASE64_STANDARD.decode(encoded_signature) {
            Ok(signature) => signature,
            Err(error) => {
                let error = ConnectorError::InvalidHeader {
                    name: signature_header.to_string(),
                    detail: error.to_string(),
                };
                return reject(
                    event_log,
                    provider,
                    style,
                    &error,
                    now,
                    Some(timestamp),
                    Some(window),
                )
                .await;
            }
        };
        if secure_eq(&expected, &provided) {
            return Ok(());
        }
    }

    let error = if any_v1 {
        ConnectorError::invalid_signature(
            "no standard-webhooks signature matched the raw request body",
        )
    } else {
        ConnectorError::InvalidHeader {
            name: signature_header.to_string(),
            detail: format!("missing `{version},` signature entry"),
        }
    };
    reject(
        event_log,
        provider,
        style,
        &error,
        now,
        Some(timestamp),
        Some(window),
    )
    .await
}

async fn ensure_timestamp_within_window<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    timestamp: OffsetDateTime,
    window: Duration,
    now: OffsetDateTime,
) -> Result<(), ConnectorError> {
    if now - timestamp > window || timestamp - now > window {
        let error = ConnectorError::TimestampOutOfWindow {
            timestamp,
            now,
            window,
        };
        return reject(
            event_log,
            provider,
            style,
            &error,
            now,
            Some(timestamp),
            Some(window),
        )
        .await;
    }
    Ok(())
}

fn required_header<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, ConnectorError> {
    header_value(headers, name).ok_or_else(|| ConnectorError::MissingHeader(name.to_string()))
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn decode_standard_webhooks_secret(secret: &str) -> Result<Vec<u8>, ConnectorError> {
    let normalized = secret.strip_prefix("whsec_").unwrap_or(secret);
    let mut padded = normalized.to_string();
    let remainder = padded.len() % 4;
    if remainder != 0 {
        padded.push_str(&"=".repeat(4 - remainder));
    }
    BASE64_STANDARD
        .decode(padded)
        .map_err(|error| ConnectorError::InvalidHeader {
            name: "webhook-secret".to_string(),
            detail: error.to_string(),
        })
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

fn secure_eq(expected: &[u8], provided: &[u8]) -> bool {
    expected.ct_eq(provided).into()
}

async fn reject<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    error: &ConnectorError,
    now: OffsetDateTime,
    signed_at: Option<OffsetDateTime>,
    window: Option<Duration>,
) -> Result<(), ConnectorError> {
    audit_rejection(event_log, provider, style, error, now, signed_at, window).await;
    Err(match error {
        ConnectorError::DuplicateProvider(value) => {
            ConnectorError::DuplicateProvider(value.clone())
        }
        ConnectorError::DuplicateDelivery(value) => {
            ConnectorError::DuplicateDelivery(value.clone())
        }
        ConnectorError::UnknownProvider(value) => ConnectorError::UnknownProvider(value.clone()),
        ConnectorError::MissingHeader(value) => ConnectorError::MissingHeader(value.clone()),
        ConnectorError::InvalidHeader { name, detail } => ConnectorError::InvalidHeader {
            name: name.clone(),
            detail: detail.clone(),
        },
        ConnectorError::InvalidSignature(value) => ConnectorError::InvalidSignature(value.clone()),
        ConnectorError::TimestampOutOfWindow {
            timestamp,
            now,
            window,
        } => ConnectorError::TimestampOutOfWindow {
            timestamp: *timestamp,
            now: *now,
            window: *window,
        },
        ConnectorError::Json(value) => ConnectorError::Json(value.clone()),
        ConnectorError::Secret(value) => ConnectorError::Secret(value.clone()),
        ConnectorError::EventLog(value) => ConnectorError::EventLog(value.clone()),
        ConnectorError::Client(value) => ConnectorError::Client(value.clone()),
        ConnectorError::Unsupported(value) => ConnectorError::Unsupported(value.clone()),
        ConnectorError::Activation(value) => ConnectorError::Activation(value.clone()),
    })
}

async fn audit_rejection<L: EventLog + ?Sized>(
    event_log: &L,
    provider: &ProviderId,
    style: HmacSignatureStyle<'_>,
    error: &ConnectorError,
    now: OffsetDateTime,
    signed_at: Option<OffsetDateTime>,
    window: Option<Duration>,
) {
    let payload = json!({
        "provider": provider.as_str(),
        "style": style.label(),
        "reason": error.to_string(),
        "observed_at": now.format(&time::format_description::well_known::Rfc3339).ok(),
        "signed_at": signed_at.and_then(|value| value.format(&time::format_description::well_known::Rfc3339).ok()),
        "window_seconds": window.map(|value| value.whole_seconds()),
    });
    let topic = Topic::new(SIGNATURE_VERIFY_AUDIT_TOPIC).expect("audit topic is valid");
    if let Err(error) = event_log
        .append(&topic, LogEvent::new("signature_rejected", payload))
        .await
    {
        crate::events::log_warn(
            "connectors.signature_verify.audit",
            &format!("failed to append signature verification audit event: {error}"),
        );
    }
}

#[cfg(test)]
mod tests {
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
            "t=12345,v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e"
                .to_string(),
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
            "t=12345,v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e"
                .to_string(),
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
}
