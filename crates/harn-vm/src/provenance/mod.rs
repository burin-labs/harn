use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{AnyEventLog, EventId, EventLog, LogError, LogEvent};
use crate::secrets::{SecretBytes, SecretError, SecretId, SecretProvider};

pub const EVENT_PROVENANCE_SCHEMA: &str = "harn-eventlog-provenance-v1";
pub const RECEIPT_SCHEMA: &str = "harn-provenance-receipt-v1";
pub const HEADER_SCHEMA: &str = "harn.provenance.schema";
pub const HEADER_PREV_HASH: &str = "harn.provenance.prev_hash";
pub const HEADER_RECORD_HASH: &str = "harn.provenance.record_hash";
const SIGNATURE_DOMAIN: &[u8] = b"harn provenance receipt v1\n";
const DEFAULT_AGENT_ID: &str = "harn-cli";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceReceipt {
    pub schema: String,
    pub receipt_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    pub producer: ReceiptProducer,
    pub run: ReceiptRun,
    pub event_log: ReceiptEventLog,
    pub chain: ReceiptChain,
    #[serde(default)]
    pub signatures: Vec<ReceiptSignature>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptProducer {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptRun {
    pub pipeline: String,
    pub status: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub exit_code: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptEventLog {
    pub backend: String,
    pub topics: Vec<String>,
    pub events: Vec<ReceiptEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptEvent {
    pub topic: String,
    pub event_id: EventId,
    pub kind: String,
    pub payload: serde_json::Value,
    pub headers: BTreeMap<String, String>,
    pub occurred_at_ms: i64,
    pub prev_hash: Option<String>,
    pub record_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptChain {
    pub algorithm: String,
    pub event_root_hash: String,
    pub receipt_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptSignature {
    pub algorithm: String,
    pub key_id: String,
    pub public_key_base64: String,
    pub signature_base64: String,
    #[serde(with = "time::serde::rfc3339")]
    pub signed_at: OffsetDateTime,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptVerificationReport {
    pub verified: bool,
    pub receipt_id: Option<String>,
    pub receipt_hash: Option<String>,
    pub event_root_hash: Option<String>,
    pub event_count: usize,
    pub signature_count: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ReceiptBuildOptions {
    pub pipeline: String,
    pub status: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub exit_code: i32,
    pub producer_name: String,
    pub producer_version: String,
}

pub fn prepare_event_for_append(
    topic: &str,
    event_id: EventId,
    previous_hash: Option<String>,
    mut event: LogEvent,
) -> Result<LogEvent, LogError> {
    event.headers.insert(
        HEADER_SCHEMA.to_string(),
        EVENT_PROVENANCE_SCHEMA.to_string(),
    );
    match previous_hash {
        Some(previous_hash) => {
            event
                .headers
                .insert(HEADER_PREV_HASH.to_string(), previous_hash);
        }
        None => {
            event.headers.remove(HEADER_PREV_HASH);
        }
    }
    event.headers.remove(HEADER_RECORD_HASH);
    let record_hash = compute_event_record_hash(topic, event_id, &event)?;
    event
        .headers
        .insert(HEADER_RECORD_HASH.to_string(), record_hash);
    Ok(event)
}

pub fn event_record_hash_from_headers(
    topic: &str,
    event_id: EventId,
    event: &LogEvent,
) -> Result<String, LogError> {
    match event.headers.get(HEADER_RECORD_HASH) {
        Some(hash) if !hash.trim().is_empty() => Ok(hash.clone()),
        _ => compute_event_record_hash(topic, event_id, event),
    }
}

pub fn compute_event_record_hash(
    topic: &str,
    event_id: EventId,
    event: &LogEvent,
) -> Result<String, LogError> {
    let mut headers = event.headers.clone();
    headers.remove(HEADER_RECORD_HASH);
    let value = serde_json::json!({
        "topic": topic,
        "event_id": event_id,
        "kind": event.kind,
        "payload": event.payload,
        "headers": headers,
        "occurred_at_ms": event.occurred_at_ms,
    });
    sha256_json("event log record hash", &value)
}

pub async fn load_or_generate_agent_signing_key(
    provider: &dyn SecretProvider,
    agent_id: Option<&str>,
) -> Result<(SigningKey, String), SecretError> {
    let agent_id = agent_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_AGENT_ID);
    let id = SecretId::new("provenance", format!("{agent_id}.ed25519.seed"));
    match provider.get(&id).await {
        Ok(secret) => {
            let seed = secret.with_exposed(decode_seed_secret)?;
            let signing_key = SigningKey::from_bytes(&seed);
            let key_id = key_id_for_verifying_key(&signing_key.verifying_key());
            Ok((signing_key, key_id))
        }
        Err(error) if secret_error_is_not_found(&error) => {
            let seed: [u8; 32] = rand::random();
            let signing_key = SigningKey::from_bytes(&seed);
            let encoded = base64::engine::general_purpose::STANDARD.encode(seed);
            provider.put(&id, SecretBytes::from(encoded)).await?;
            let key_id = key_id_for_verifying_key(&signing_key.verifying_key());
            Ok((signing_key, key_id))
        }
        Err(error) => Err(error),
    }
}

pub async fn build_signed_receipt(
    log: &Arc<AnyEventLog>,
    options: ReceiptBuildOptions,
    signing_key: &SigningKey,
    key_id: String,
) -> Result<ProvenanceReceipt, LogError> {
    let description = log.describe();
    let mut topics = log.topics().await?;
    topics.sort_by(|left, right| left.as_str().cmp(right.as_str()));

    let mut topic_names = Vec::with_capacity(topics.len());
    let mut events = Vec::new();
    for topic in topics {
        topic_names.push(topic.as_str().to_string());
        for (event_id, event) in log.read_range(&topic, None, usize::MAX).await? {
            let record_hash = event_record_hash_from_headers(topic.as_str(), event_id, &event)?;
            let prev_hash = event.headers.get(HEADER_PREV_HASH).cloned();
            events.push(ReceiptEvent {
                topic: topic.as_str().to_string(),
                event_id,
                kind: event.kind,
                payload: event.payload,
                headers: event.headers,
                occurred_at_ms: event.occurred_at_ms,
                prev_hash,
                record_hash,
            });
        }
    }
    events.sort_by(|left, right| {
        left.topic
            .cmp(&right.topic)
            .then(left.event_id.cmp(&right.event_id))
    });
    let event_root_hash = merkle_root(events.iter().map(|event| event.record_hash.as_str()));
    let mut receipt = ProvenanceReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        receipt_id: format!("receipt-{}", Uuid::now_v7()),
        issued_at: OffsetDateTime::now_utc(),
        producer: ReceiptProducer {
            name: options.producer_name,
            version: options.producer_version,
        },
        run: ReceiptRun {
            pipeline: options.pipeline,
            status: options.status,
            started_at_ms: options.started_at_ms,
            finished_at_ms: options.finished_at_ms,
            exit_code: options.exit_code,
        },
        event_log: ReceiptEventLog {
            backend: description.backend.to_string(),
            topics: topic_names,
            events,
        },
        chain: ReceiptChain {
            algorithm: "sha256/ed25519".to_string(),
            event_root_hash,
            receipt_hash: String::new(),
        },
        signatures: Vec::new(),
    };
    let canonical = canonical_unsigned_receipt(&receipt)?;
    let receipt_hash = sha256_bytes_prefixed(&canonical);
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN.len() + canonical.len());
    message.extend_from_slice(SIGNATURE_DOMAIN);
    message.extend_from_slice(canonical.as_bytes());
    let signature = signing_key.sign(&message);
    let verifying_key = signing_key.verifying_key();
    receipt.chain.receipt_hash = receipt_hash;
    receipt.signatures.push(ReceiptSignature {
        algorithm: "ed25519".to_string(),
        key_id,
        public_key_base64: base64::engine::general_purpose::STANDARD
            .encode(verifying_key.to_bytes()),
        signature_base64: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        signed_at: OffsetDateTime::now_utc(),
    });
    Ok(receipt)
}

pub fn verify_receipt(receipt: &ProvenanceReceipt) -> ReceiptVerificationReport {
    let mut report = ReceiptVerificationReport {
        receipt_id: Some(receipt.receipt_id.clone()),
        receipt_hash: Some(receipt.chain.receipt_hash.clone()),
        event_root_hash: Some(receipt.chain.event_root_hash.clone()),
        event_count: receipt.event_log.events.len(),
        signature_count: receipt.signatures.len(),
        ..ReceiptVerificationReport::default()
    };

    if receipt.schema != RECEIPT_SCHEMA {
        report
            .errors
            .push(format!("unsupported receipt schema '{}'", receipt.schema));
    }
    verify_receipt_events(receipt, &mut report);
    verify_receipt_hash(receipt, &mut report);
    verify_receipt_signatures(receipt, &mut report);
    report.verified = report.errors.is_empty();
    report
}

fn verify_receipt_events(receipt: &ProvenanceReceipt, report: &mut ReceiptVerificationReport) {
    let mut by_topic: HashMap<&str, Vec<&ReceiptEvent>> = HashMap::new();
    for event in &receipt.event_log.events {
        by_topic
            .entry(event.topic.as_str())
            .or_default()
            .push(event);
    }

    let mut event_hashes = Vec::with_capacity(receipt.event_log.events.len());
    for (topic, events) in by_topic.iter_mut() {
        events.sort_by_key(|event| event.event_id);
        let mut previous_hash: Option<String> = None;
        for event in events.iter() {
            if event.prev_hash != previous_hash {
                report.errors.push(format!(
                    "topic {topic} event {} prev_hash mismatch; expected {:?}, found {:?}",
                    event.event_id, previous_hash, event.prev_hash
                ));
            }
            let header_prev = event.headers.get(HEADER_PREV_HASH).cloned();
            if header_prev != event.prev_hash {
                report.errors.push(format!(
                    "topic {topic} event {} prev_hash does not match provenance header",
                    event.event_id
                ));
            }
            let header_hash = event.headers.get(HEADER_RECORD_HASH);
            if header_hash != Some(&event.record_hash) {
                report.errors.push(format!(
                    "topic {topic} event {} record_hash does not match provenance header",
                    event.event_id
                ));
            }
            let log_event = LogEvent {
                kind: event.kind.clone(),
                payload: event.payload.clone(),
                headers: event.headers.clone(),
                occurred_at_ms: event.occurred_at_ms,
            };
            match compute_event_record_hash(topic, event.event_id, &log_event) {
                Ok(expected) if expected == event.record_hash => {}
                Ok(expected) => report.errors.push(format!(
                    "topic {topic} event {} record_hash mismatch; expected {expected}, found {}",
                    event.event_id, event.record_hash
                )),
                Err(error) => report.errors.push(format!(
                    "topic {topic} event {} hash error: {error}",
                    event.event_id
                )),
            }
            previous_hash = Some(event.record_hash.clone());
            event_hashes.push((
                event.topic.as_str(),
                event.event_id,
                event.record_hash.as_str(),
            ));
        }
    }

    event_hashes.sort_by(|left, right| left.0.cmp(right.0).then(left.1.cmp(&right.1)));
    let expected_root = merkle_root(event_hashes.iter().map(|(_, _, hash)| *hash));
    if expected_root != receipt.chain.event_root_hash {
        report.errors.push(format!(
            "event_root_hash mismatch; expected {expected_root}, found {}",
            receipt.chain.event_root_hash
        ));
    }
}

fn verify_receipt_hash(receipt: &ProvenanceReceipt, report: &mut ReceiptVerificationReport) {
    match canonical_unsigned_receipt(receipt) {
        Ok(canonical) => {
            let expected = sha256_bytes_prefixed(&canonical);
            if expected != receipt.chain.receipt_hash {
                report.errors.push(format!(
                    "receipt_hash mismatch; expected {expected}, found {}",
                    receipt.chain.receipt_hash
                ));
            }
        }
        Err(error) => report
            .errors
            .push(format!("receipt canonicalization failed: {error}")),
    }
}

fn verify_receipt_signatures(receipt: &ProvenanceReceipt, report: &mut ReceiptVerificationReport) {
    if receipt.signatures.is_empty() {
        report.errors.push("receipt has no signatures".to_string());
        return;
    }
    let canonical = match canonical_unsigned_receipt(receipt) {
        Ok(canonical) => canonical,
        Err(error) => {
            report
                .errors
                .push(format!("receipt canonicalization failed: {error}"));
            return;
        }
    };
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN.len() + canonical.len());
    message.extend_from_slice(SIGNATURE_DOMAIN);
    message.extend_from_slice(canonical.as_bytes());
    let mut any_valid = false;
    for signature in &receipt.signatures {
        if signature.algorithm != "ed25519" {
            report.errors.push(format!(
                "signature {} uses unsupported algorithm '{}'",
                signature.key_id, signature.algorithm
            ));
            continue;
        }
        let public_key = match base64::engine::general_purpose::STANDARD
            .decode(signature.public_key_base64.as_bytes())
        {
            Ok(bytes) => bytes,
            Err(error) => {
                report.errors.push(format!(
                    "signature {} public key is not base64: {error}",
                    signature.key_id
                ));
                continue;
            }
        };
        let Ok(public_key) = <[u8; 32]>::try_from(public_key.as_slice()) else {
            report.errors.push(format!(
                "signature {} public key must be 32 bytes",
                signature.key_id
            ));
            continue;
        };
        let verifying_key = match VerifyingKey::from_bytes(&public_key) {
            Ok(key) => key,
            Err(error) => {
                report.errors.push(format!(
                    "signature {} public key is invalid: {error}",
                    signature.key_id
                ));
                continue;
            }
        };
        let signature_bytes = match base64::engine::general_purpose::STANDARD
            .decode(signature.signature_base64.as_bytes())
        {
            Ok(bytes) => bytes,
            Err(error) => {
                report.errors.push(format!(
                    "signature {} bytes are not base64: {error}",
                    signature.key_id
                ));
                continue;
            }
        };
        let parsed_signature = match Signature::from_slice(&signature_bytes) {
            Ok(signature) => signature,
            Err(error) => {
                report.errors.push(format!(
                    "signature {} bytes are invalid: {error}",
                    signature.key_id
                ));
                continue;
            }
        };
        match verifying_key.verify(&message, &parsed_signature) {
            Ok(()) => any_valid = true,
            Err(error) => report.errors.push(format!(
                "signature {} failed verification: {error}",
                signature.key_id
            )),
        }
    }
    if !any_valid {
        report
            .errors
            .push("no valid receipt signature found".to_string());
    }
}

fn canonical_unsigned_receipt(receipt: &ProvenanceReceipt) -> Result<String, LogError> {
    let mut unsigned = receipt.clone();
    unsigned.chain.receipt_hash.clear();
    unsigned.signatures.clear();
    serde_json::to_string(&unsigned)
        .map_err(|error| LogError::Serde(format!("receipt canonicalize error: {error}")))
}

fn sha256_json(context: &str, value: &serde_json::Value) -> Result<String, LogError> {
    let canonical = serde_json::to_string(value)
        .map_err(|error| LogError::Serde(format!("{context} canonicalize error: {error}")))?;
    Ok(sha256_bytes_prefixed(&canonical))
}

fn sha256_bytes_prefixed(bytes: &str) -> String {
    let digest = Sha256::digest(bytes.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

fn merkle_root<'a>(hashes: impl Iterator<Item = &'a str>) -> String {
    let mut level = hashes.map(str::to_string).collect::<Vec<_>>();
    if level.is_empty() {
        return sha256_bytes_prefixed("");
    }
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let right = pair.get(1).unwrap_or(&pair[0]);
            next.push(sha256_bytes_prefixed(&format!("{}{}", pair[0], right)));
        }
        level = next;
    }
    level.pop().expect("non-empty merkle level")
}

fn decode_seed_secret(bytes: &[u8]) -> Result<[u8; 32], SecretError> {
    let text = std::str::from_utf8(bytes).map_err(|error| SecretError::Backend {
        provider: "provenance".to_string(),
        message: format!("stored Ed25519 seed is not UTF-8: {error}"),
    })?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(text.trim().as_bytes())
        .map_err(|error| SecretError::Backend {
            provider: "provenance".to_string(),
            message: format!("stored Ed25519 seed is not base64: {error}"),
        })?;
    <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| SecretError::Backend {
        provider: "provenance".to_string(),
        message: "stored Ed25519 seed must decode to 32 bytes".to_string(),
    })
}

fn key_id_for_verifying_key(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.to_bytes());
    format!("ed25519:{}", hex::encode(&digest[..16]))
}

fn secret_error_is_not_found(error: &SecretError) -> bool {
    match error {
        SecretError::NotFound { .. } => true,
        SecretError::All(errors) => errors.iter().all(secret_error_is_not_found),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{EventLog, MemoryEventLog, Topic};

    #[tokio::test]
    async fn receipt_verifies_and_detects_event_tamper() {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let topic = Topic::new("run.provenance").unwrap();
        log.append(
            &topic,
            LogEvent::new("started", serde_json::json!({"pipeline": "main"})),
        )
        .await
        .unwrap();
        log.append(
            &topic,
            LogEvent::new("finished", serde_json::json!({"status": "ok"})),
        )
        .await
        .unwrap();
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let receipt = build_signed_receipt(
            &log,
            ReceiptBuildOptions {
                pipeline: "main.harn".to_string(),
                status: "ok".to_string(),
                started_at_ms: 1,
                finished_at_ms: 2,
                exit_code: 0,
                producer_name: "harn".to_string(),
                producer_version: "test".to_string(),
            },
            &signing_key,
            "test-key".to_string(),
        )
        .await
        .unwrap();
        assert!(verify_receipt(&receipt).verified);

        let mut tampered = receipt;
        tampered.event_log.events[0].payload = serde_json::json!({"pipeline": "other"});
        let report = verify_receipt(&tampered);
        assert!(!report.verified);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("record_hash mismatch")));
    }
}
