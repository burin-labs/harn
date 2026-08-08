use std::collections::BTreeMap;

use time::{Duration, OffsetDateTime};

use super::hmac::{hmac_sha256, required_header, secure_eq, DEFAULT_STRIPE_SIGNATURE_HEADER};
use super::ConnectorError;

/// Verify a Stripe webhook signature without requiring runtime state.
///
/// All `v1` signatures are checked so endpoint-secret rotation works without
/// special handling by callers.
pub fn verify_stripe_signature(
    body: &[u8],
    headers: &BTreeMap<String, String>,
    secret: &str,
    timestamp_window: Duration,
    now: OffsetDateTime,
) -> Result<(), ConnectorError> {
    verify_stripe_signature_with(
        body,
        headers,
        secret,
        timestamp_window,
        now,
        DEFAULT_STRIPE_SIGNATURE_HEADER,
        "v1",
    )
}

pub(super) fn verify_stripe_signature_with(
    body: &[u8],
    headers: &BTreeMap<String, String>,
    secret: &str,
    timestamp_window: Duration,
    now: OffsetDateTime,
    signature_header: &str,
    version: &str,
) -> Result<(), ConnectorError> {
    let header = required_header(headers, signature_header)?;
    let mut timestamp = None;
    let mut provided = Vec::new();
    for part in header.split(',') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "t" {
            let raw = value
                .parse::<i64>()
                .map_err(|error| ConnectorError::InvalidHeader {
                    name: signature_header.to_string(),
                    detail: error.to_string(),
                })?;
            timestamp = Some(OffsetDateTime::from_unix_timestamp(raw).map_err(|error| {
                ConnectorError::InvalidHeader {
                    name: signature_header.to_string(),
                    detail: error.to_string(),
                }
            })?);
        } else if key == version {
            provided.push(
                hex::decode(value).map_err(|error| ConnectorError::InvalidHeader {
                    name: signature_header.to_string(),
                    detail: error.to_string(),
                })?,
            );
        }
    }

    let timestamp = timestamp.ok_or_else(|| ConnectorError::InvalidHeader {
        name: signature_header.to_string(),
        detail: "missing `t=` timestamp component".to_string(),
    })?;
    if now - timestamp > timestamp_window || timestamp - now > timestamp_window {
        return Err(ConnectorError::TimestampOutOfWindow {
            timestamp,
            now,
            window: timestamp_window,
        });
    }
    if provided.is_empty() {
        return Err(ConnectorError::InvalidHeader {
            name: signature_header.to_string(),
            detail: format!("missing `{version}=` signature component"),
        });
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
        Err(ConnectorError::invalid_signature(
            "no stripe signature matched the raw request body",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_any_matching_rotation_signature() {
        let headers = BTreeMap::from([(
            "Stripe-Signature".to_string(),
            concat!(
                "t=12345,",
                "v1=0000000000000000000000000000000000000000000000000000000000000000,",
                "v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e"
            )
            .to_string(),
        )]);

        verify_stripe_signature(
            b"{\n  \"id\": \"evt_test_webhook\",\n  \"object\": \"event\"\n}",
            &headers,
            "whsec_test_secret",
            Duration::seconds(30),
            OffsetDateTime::from_unix_timestamp(12_350).unwrap(),
        )
        .unwrap();
    }
}
