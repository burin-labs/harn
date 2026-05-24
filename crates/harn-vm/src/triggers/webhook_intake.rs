//! Forge-agnostic webhook intake substrate.
//!
//! This is the lowest-level layer connectors can wire into to absorb webhook
//! deliveries. It is deliberately ignorant of any specific provider (GitHub,
//! GitLab, Linear, Slack, Stripe, ...). A connector declares:
//!
//! * a path scope (e.g. `/hooks/github`)
//! * a signature header + algorithm + format the substrate should verify
//! * a delivery-id header the substrate should dedupe on
//! * a topic to republish accepted deliveries onto
//!
//! and the substrate handles the rest: HMAC verification, delivery-id
//! deduplication (durable across process restarts via the trigger inbox), and
//! republishing to the chosen event-log topic. Per-forge event normalization
//! lives in the connector that consumes the topic.
//!
//! The substrate is process-global, mirroring the rest of the trigger runtime.
//! It is exposed to Harn scripts through the `webhook_intake_*` builtins.
//
// Design notes:
// - A connector should be able to wire intake in <30 lines of Harn (issue
//   #1011). The Harn-facing surface is therefore a single `register` call that
//   takes a config dict, plus `feed` for hand-driving the substrate from a
//   handler or a test fixture.
// - Dedupe survives process restart because we delegate to the existing
//   `InboxIndex`, which rehydrates from the durable claim topic on construction.
// - Multiple connectors on different paths must not collide — each intake gets
//   its own intake id, and the inbox dedupe is namespaced by intake id.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::connectors::MetricsRegistry;
use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::runtime_limits::RuntimeLimits;
use crate::triggers::inbox::InboxIndex;

/// Default delivery-id retention used when callers omit the field. Matches the
/// 24-hour window most providers retry within.
pub const DEFAULT_DEDUPE_TTL_SECS: u64 = 24 * 60 * 60;
/// Topic the substrate appends `webhook_intake_rejected` audit events on.
pub const REJECTION_TOPIC: &str = "triggers.webhook_intake.rejections";
const INTAKE_EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;

/// Identifier of a registered intake. Stable for the lifetime of the process,
/// or until `webhook_intake_deregister` is called.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WebhookIntakeId(pub String);

impl WebhookIntakeId {
    pub fn new() -> Self {
        Self(format!("intake_{}", Uuid::now_v7()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for WebhookIntakeId {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash algorithm used to recompute the HMAC over the request body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HmacAlgorithm {
    #[default]
    Sha256,
    Sha1,
}

impl HmacAlgorithm {
    pub fn parse(raw: &str) -> Option<Self> {
        Self::parse_with_legacy_sha1(raw, false)
    }

    pub fn parse_with_legacy_sha1(raw: &str, allow_legacy_sha1: bool) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "sha256" | "hmac-sha256" => Some(Self::Sha256),
            "sha1" | "hmac-sha1" if allow_legacy_sha1 => Some(Self::Sha1),
            _ => None,
        }
    }

    pub fn is_legacy_sha1_alias(raw: &str) -> bool {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "sha1" | "hmac-sha1"
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
        }
    }
}

/// Encoding the wire signature is presented in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureEncoding {
    #[default]
    Hex,
    Base64,
}

impl SignatureEncoding {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "hex" | "base16" => Some(Self::Hex),
            "base64" | "b64" => Some(Self::Base64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::Base64 => "base64",
        }
    }
}

/// Configuration for a single registered intake.
#[derive(Clone, Debug)]
pub struct WebhookIntakeConfig {
    pub id: Option<String>,
    pub path: Option<String>,
    pub signature_header: String,
    pub signature_prefix: Option<String>,
    pub signature_encoding: SignatureEncoding,
    pub algorithm: HmacAlgorithm,
    pub allow_legacy_sha1: bool,
    pub secret: Vec<u8>,
    pub delivery_id_header: String,
    pub topic: String,
    pub dedupe_ttl: StdDuration,
}

impl WebhookIntakeConfig {
    /// Common forges (GitHub, GitLab, Bitbucket) prefix the digest with the
    /// algorithm name. Helper for callers that want the same default the
    /// stdlib parser applies when `signature_prefix` is omitted.
    pub fn default_prefix(algorithm: HmacAlgorithm) -> Option<String> {
        Some(format!("{}=", algorithm.as_str()))
    }
}

/// What the substrate did with a delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookIntakeStatus {
    Accepted,
    Duplicate,
    Rejected,
}

impl WebhookIntakeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Rejected => "rejected",
        }
    }
}

/// Result of feeding a delivery through the substrate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookIntakeOutcome {
    pub status: String,
    pub intake_id: String,
    pub topic: String,
    pub delivery_id: Option<String>,
    pub topic_event_id: Option<u64>,
    pub reason: Option<String>,
    pub received_at: String,
}

/// Snapshot of an intake registration. Returned by `register` and `list`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookIntakeSnapshot {
    pub id: String,
    pub path: Option<String>,
    pub topic: String,
    pub signature_header: String,
    pub signature_prefix: Option<String>,
    pub signature_encoding: String,
    pub algorithm: String,
    pub allow_legacy_sha1: bool,
    pub delivery_id_header: String,
    pub dedupe_ttl_seconds: u64,
}

/// Raw inbound delivery as fed to the substrate.
#[derive(Clone, Debug, Default)]
pub struct WebhookIntakeRequest {
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub path: Option<String>,
    pub received_at: Option<OffsetDateTime>,
}

#[derive(Debug)]
pub enum WebhookIntakeError {
    Config(String),
    UnknownIntake(String),
    Internal(String),
}

impl std::fmt::Display for WebhookIntakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(detail) => write!(f, "intake config: {detail}"),
            Self::UnknownIntake(id) => write!(f, "intake `{id}` is not registered"),
            Self::Internal(detail) => write!(f, "intake substrate: {detail}"),
        }
    }
}

impl std::error::Error for WebhookIntakeError {}

#[derive(Clone)]
struct RegisteredIntake {
    snapshot: WebhookIntakeSnapshot,
    config: WebhookIntakeConfig,
}

#[derive(Default)]
struct WebhookIntakeRegistry {
    by_id: HashMap<String, RegisteredIntake>,
    by_path: HashMap<String, String>,
}

thread_local! {
    static REGISTRY: RefCell<WebhookIntakeRegistry> =
        RefCell::new(WebhookIntakeRegistry::default());
}

fn with_registry<R>(f: impl FnOnce(&WebhookIntakeRegistry) -> R) -> R {
    REGISTRY.with(|slot| f(&slot.borrow()))
}

fn with_registry_mut<R>(f: impl FnOnce(&mut WebhookIntakeRegistry) -> R) -> R {
    REGISTRY.with(|slot| f(&mut slot.borrow_mut()))
}

/// Reset all in-process intake state. Called between conformance tests.
pub fn clear_webhook_intake_state() {
    with_registry_mut(|state| {
        state.by_id.clear();
        state.by_path.clear();
    });
    reset_cached_inbox();
}

/// Snapshot of all currently-registered intakes.
pub fn snapshot_webhook_intakes() -> Vec<WebhookIntakeSnapshot> {
    with_registry(|state| {
        let mut out: Vec<_> = state
            .by_id
            .values()
            .map(|entry| entry.snapshot.clone())
            .collect();
        out.sort_by(|left, right| left.id.cmp(&right.id));
        out
    })
}

/// Resolve the intake registered against `path`, if any.
pub fn intake_for_path(path: &str) -> Option<String> {
    with_registry(|state| state.by_path.get(path).cloned())
}

/// Register a new intake. Returns its assigned id.
pub fn register_webhook_intake(
    config: WebhookIntakeConfig,
) -> Result<WebhookIntakeSnapshot, WebhookIntakeError> {
    if config.signature_header.trim().is_empty() {
        return Err(WebhookIntakeError::Config(
            "signature_header cannot be empty".to_string(),
        ));
    }
    if config.delivery_id_header.trim().is_empty() {
        return Err(WebhookIntakeError::Config(
            "delivery_id_header cannot be empty".to_string(),
        ));
    }
    if config.topic.trim().is_empty() {
        return Err(WebhookIntakeError::Config(
            "topic cannot be empty".to_string(),
        ));
    }
    if config.secret.is_empty() {
        return Err(WebhookIntakeError::Config(
            "secret cannot be empty".to_string(),
        ));
    }
    if config.algorithm == HmacAlgorithm::Sha1 && !config.allow_legacy_sha1 {
        return Err(WebhookIntakeError::Config(
            "algorithm `sha1` is legacy; set `allow_legacy_sha1: true` to verify SHA-1 HMAC signatures for an existing provider".to_string(),
        ));
    }
    Topic::new(config.topic.clone())
        .map_err(|error| WebhookIntakeError::Config(format!("invalid topic: {error}")))?;

    let id = config
        .id
        .clone()
        .filter(|raw| !raw.trim().is_empty())
        .unwrap_or_else(|| WebhookIntakeId::new().0);

    with_registry_mut(|state| {
        if state.by_id.contains_key(&id) {
            return Err(WebhookIntakeError::Config(format!(
                "intake `{id}` is already registered"
            )));
        }
        if let Some(path) = config.path.as_ref() {
            if state.by_path.contains_key(path) {
                return Err(WebhookIntakeError::Config(format!(
                    "path `{path}` is already bound to intake `{}`",
                    state.by_path[path]
                )));
            }
        }

        let snapshot = WebhookIntakeSnapshot {
            id: id.clone(),
            path: config.path.clone(),
            topic: config.topic.clone(),
            signature_header: normalize_header(&config.signature_header),
            signature_prefix: config.signature_prefix.clone(),
            signature_encoding: config.signature_encoding.as_str().to_string(),
            algorithm: config.algorithm.as_str().to_string(),
            allow_legacy_sha1: config.allow_legacy_sha1,
            delivery_id_header: normalize_header(&config.delivery_id_header),
            dedupe_ttl_seconds: config.dedupe_ttl.as_secs(),
        };

        let mut stored = config;
        stored.id = Some(id.clone());
        stored.signature_header = normalize_header(&stored.signature_header);
        stored.delivery_id_header = normalize_header(&stored.delivery_id_header);

        if let Some(path) = stored.path.as_ref() {
            state.by_path.insert(path.clone(), id.clone());
        }
        state.by_id.insert(
            id,
            RegisteredIntake {
                snapshot: snapshot.clone(),
                config: stored,
            },
        );
        Ok(snapshot)
    })
}

/// Remove an intake. Returns `true` if it existed.
pub fn deregister_webhook_intake(intake_id: &str) -> bool {
    with_registry_mut(|state| {
        let Some(entry) = state.by_id.remove(intake_id) else {
            return false;
        };
        if let Some(path) = entry.snapshot.path {
            state.by_path.remove(&path);
        }
        true
    })
}

/// Feed a delivery through the substrate.
///
/// On `Accepted`, the substrate has appended a `webhook_delivery` event to the
/// configured topic and claimed the delivery id in the inbox. Subsequent calls
/// with the same delivery id within the dedupe TTL return `Duplicate`. Any
/// signature failure (missing/invalid header, bad HMAC) returns `Rejected`.
pub async fn feed_webhook_intake(
    intake_id: &str,
    request: WebhookIntakeRequest,
) -> Result<WebhookIntakeOutcome, WebhookIntakeError> {
    let entry = with_registry(|state| state.by_id.get(intake_id).cloned())
        .ok_or_else(|| WebhookIntakeError::UnknownIntake(intake_id.to_string()))?;
    let received_at = request.received_at.unwrap_or_else(OffsetDateTime::now_utc);
    let received_at_str = format_rfc3339(received_at);
    let log = ensure_event_log();
    let topic = Topic::new(entry.snapshot.topic.clone())
        .map_err(|error| WebhookIntakeError::Internal(format!("invalid topic: {error}")))?;

    if let Some(expected_path) = entry.config.path.as_deref() {
        if let Some(actual_path) = request.path.as_deref() {
            if actual_path != expected_path {
                let reason =
                    format!("path mismatch: expected `{expected_path}`, got `{actual_path}`");
                emit_rejection(&log, &entry.snapshot, &reason, &received_at_str).await;
                return Ok(WebhookIntakeOutcome {
                    status: WebhookIntakeStatus::Rejected.as_str().to_string(),
                    intake_id: entry.snapshot.id.clone(),
                    topic: entry.snapshot.topic.clone(),
                    delivery_id: None,
                    topic_event_id: None,
                    reason: Some(reason),
                    received_at: received_at_str,
                });
            }
        }
    }

    let signature = match find_header(&request.headers, &entry.config.signature_header) {
        Some(value) => value,
        None => {
            let reason = format!(
                "missing signature header `{}`",
                entry.config.signature_header
            );
            emit_rejection(&log, &entry.snapshot, &reason, &received_at_str).await;
            return Ok(WebhookIntakeOutcome {
                status: WebhookIntakeStatus::Rejected.as_str().to_string(),
                intake_id: entry.snapshot.id.clone(),
                topic: entry.snapshot.topic.clone(),
                delivery_id: None,
                topic_event_id: None,
                reason: Some(reason),
                received_at: received_at_str,
            });
        }
    };

    if let Err(reason) = verify_signature(&entry.config, signature.as_str(), &request.body) {
        emit_rejection(&log, &entry.snapshot, &reason, &received_at_str).await;
        return Ok(WebhookIntakeOutcome {
            status: WebhookIntakeStatus::Rejected.as_str().to_string(),
            intake_id: entry.snapshot.id.clone(),
            topic: entry.snapshot.topic.clone(),
            delivery_id: None,
            topic_event_id: None,
            reason: Some(reason),
            received_at: received_at_str,
        });
    }

    let delivery_id = match find_header(&request.headers, &entry.config.delivery_id_header) {
        Some(value) if !value.trim().is_empty() => value,
        _ => {
            let reason = format!(
                "missing delivery id header `{}`",
                entry.config.delivery_id_header
            );
            emit_rejection(&log, &entry.snapshot, &reason, &received_at_str).await;
            return Ok(WebhookIntakeOutcome {
                status: WebhookIntakeStatus::Rejected.as_str().to_string(),
                intake_id: entry.snapshot.id.clone(),
                topic: entry.snapshot.topic.clone(),
                delivery_id: None,
                topic_event_id: None,
                reason: Some(reason),
                received_at: received_at_str,
            });
        }
    };

    let inbox = ensure_inbox().await?;
    let claim_key = format!("intake:{}", entry.snapshot.id);
    let claimed = inbox
        .insert_if_new(&claim_key, &delivery_id, entry.config.dedupe_ttl)
        .await
        .map_err(|error| WebhookIntakeError::Internal(format!("inbox claim: {error}")))?;
    if !claimed {
        return Ok(WebhookIntakeOutcome {
            status: WebhookIntakeStatus::Duplicate.as_str().to_string(),
            intake_id: entry.snapshot.id.clone(),
            topic: entry.snapshot.topic.clone(),
            delivery_id: Some(delivery_id),
            topic_event_id: None,
            reason: None,
            received_at: received_at_str,
        });
    }

    let payload = serde_json::json!({
        "intake_id": entry.snapshot.id,
        "delivery_id": delivery_id,
        "received_at": received_at_str,
        "headers": request.headers,
        "body_b64": BASE64_STANDARD.encode(&request.body),
        "body_text": std::str::from_utf8(&request.body).ok().map(str::to_string),
        "path": entry.config.path,
        "signature_header": entry.config.signature_header,
        "delivery_id_header": entry.config.delivery_id_header,
        "algorithm": entry.config.algorithm.as_str(),
    });
    let mut headers_meta = BTreeMap::new();
    headers_meta.insert("intake_id".to_string(), entry.snapshot.id.clone());
    headers_meta.insert("delivery_id".to_string(), delivery_id.clone());
    let event_id = log
        .append(
            &topic,
            LogEvent::new("webhook_delivery", payload).with_headers(headers_meta),
        )
        .await
        .map_err(|error| WebhookIntakeError::Internal(format!("topic append: {error}")))?;

    Ok(WebhookIntakeOutcome {
        status: WebhookIntakeStatus::Accepted.as_str().to_string(),
        intake_id: entry.snapshot.id.clone(),
        topic: entry.snapshot.topic.clone(),
        delivery_id: Some(delivery_id),
        topic_event_id: Some(event_id),
        reason: None,
        received_at: received_at_str,
    })
}

/// Read the most recent `limit` accepted deliveries on an intake's topic.
/// Provides the bounded replay buffer surface called out in #1011.
pub async fn recent_webhook_deliveries(
    intake_id: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, WebhookIntakeError> {
    let topic_name = with_registry(|state| {
        state
            .by_id
            .get(intake_id)
            .map(|entry| entry.snapshot.topic.clone())
    })
    .ok_or_else(|| WebhookIntakeError::UnknownIntake(intake_id.to_string()))?;
    let log = ensure_event_log();
    let topic = Topic::new(topic_name)
        .map_err(|error| WebhookIntakeError::Internal(format!("invalid topic: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| WebhookIntakeError::Internal(format!("topic read: {error}")))?;
    let mut payloads: Vec<serde_json::Value> = events
        .into_iter()
        .filter(|(_, event)| {
            event.kind == "webhook_delivery"
                && event
                    .headers
                    .get("intake_id")
                    .map(|value| value == intake_id)
                    .unwrap_or(true)
        })
        .map(|(_, event)| event.payload)
        .collect();
    if payloads.len() > limit {
        let drop = payloads.len() - limit;
        payloads.drain(0..drop);
    }
    Ok(payloads)
}

fn verify_signature(
    config: &WebhookIntakeConfig,
    raw_signature: &str,
    body: &[u8],
) -> Result<(), String> {
    let stripped = strip_prefix(raw_signature, config.signature_prefix.as_deref())?;
    let provided = decode_signature(stripped, config.signature_encoding)?;
    let expected = compute_hmac(config.algorithm, &config.secret, body);
    if provided.len() != expected.len() {
        return Err(format!(
            "signature length mismatch (expected {} bytes, got {})",
            expected.len(),
            provided.len()
        ));
    }
    if expected.ct_eq(&provided).into() {
        Ok(())
    } else {
        Err("signature mismatch".to_string())
    }
}

fn strip_prefix<'a>(raw: &'a str, prefix: Option<&str>) -> Result<&'a str, String> {
    let trimmed = raw.trim();
    match prefix {
        Some(expected) if !expected.is_empty() => trimmed
            .strip_prefix(expected)
            .ok_or_else(|| format!("signature missing expected prefix `{expected}`")),
        _ => Ok(trimmed),
    }
}

fn decode_signature(raw: &str, encoding: SignatureEncoding) -> Result<Vec<u8>, String> {
    match encoding {
        SignatureEncoding::Hex => hex::decode(raw).map_err(|error| format!("invalid hex: {error}")),
        SignatureEncoding::Base64 => BASE64_STANDARD
            .decode(raw)
            .map_err(|error| format!("invalid base64: {error}")),
    }
}

fn compute_hmac(algorithm: HmacAlgorithm, secret: &[u8], data: &[u8]) -> Vec<u8> {
    match algorithm {
        HmacAlgorithm::Sha256 => crate::connectors::hmac::hmac_sha256(secret, data),
        HmacAlgorithm::Sha1 => crate::connectors::hmac::hmac_sha1(secret, data),
    }
}

fn find_header(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    headers
        .iter()
        .find(|(key, _)| key.to_ascii_lowercase() == lower)
        .map(|(_, value)| value.clone())
}

fn normalize_header(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn format_rfc3339(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_default()
}

fn ensure_event_log() -> Arc<crate::event_log::AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(INTAKE_EVENT_LOG_QUEUE_DEPTH))
}

thread_local! {
    static CACHED_INBOX: RefCell<Option<(Arc<crate::event_log::AnyEventLog>, Arc<InboxIndex>)>> =
        const { RefCell::new(None) };
}

async fn ensure_inbox() -> Result<Arc<InboxIndex>, WebhookIntakeError> {
    let log = ensure_event_log();
    if let Some(existing) = CACHED_INBOX.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|(cached_log, _)| Arc::ptr_eq(cached_log, &log))
            .map(|(_, inbox)| inbox.clone())
    }) {
        return Ok(existing);
    }
    let metrics = Arc::new(MetricsRegistry::default());
    let inbox = Arc::new(
        InboxIndex::new(log.clone(), metrics)
            .await
            .map_err(|error| WebhookIntakeError::Internal(format!("inbox init: {error}")))?,
    );
    CACHED_INBOX.with(|slot| {
        *slot.borrow_mut() = Some((log, inbox.clone()));
    });
    Ok(inbox)
}

fn reset_cached_inbox() {
    CACHED_INBOX.with(|slot| *slot.borrow_mut() = None);
}

async fn emit_rejection(
    log: &Arc<crate::event_log::AnyEventLog>,
    snapshot: &WebhookIntakeSnapshot,
    reason: &str,
    received_at: &str,
) {
    let topic = match Topic::new(REJECTION_TOPIC) {
        Ok(topic) => topic,
        Err(_) => return,
    };
    let mut headers = BTreeMap::new();
    headers.insert("intake_id".to_string(), snapshot.id.clone());
    let payload = serde_json::json!({
        "intake_id": snapshot.id,
        "topic": snapshot.topic,
        "path": snapshot.path,
        "reason": reason,
        "received_at": received_at,
    });
    let _ = log
        .append(
            &topic,
            LogEvent::new("webhook_intake_rejected", payload).with_headers(headers),
        )
        .await;
}

/// Helper: build a `WebhookIntakeRequest` from a `headers` dict, body, and
/// optional fields. Used by the stdlib bindings to keep the parsing logic out
/// of the substrate proper.
pub fn build_request(
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    path: Option<String>,
    received_at: Option<OffsetDateTime>,
) -> WebhookIntakeRequest {
    WebhookIntakeRequest {
        headers,
        body,
        path,
        received_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{install_default_for_base_dir, AnyEventLog, FileEventLog};

    fn make_config(secret: &[u8]) -> WebhookIntakeConfig {
        WebhookIntakeConfig {
            id: None,
            path: None,
            signature_header: "x-test-signature".to_string(),
            signature_prefix: Some("sha256=".to_string()),
            signature_encoding: SignatureEncoding::Hex,
            algorithm: HmacAlgorithm::Sha256,
            allow_legacy_sha1: false,
            secret: secret.to_vec(),
            delivery_id_header: "x-test-delivery".to_string(),
            topic: "tests.webhook_intake".to_string(),
            dedupe_ttl: StdDuration::from_secs(60),
        }
    }

    fn signed_headers(
        config: &WebhookIntakeConfig,
        body: &[u8],
        delivery_id: &str,
    ) -> BTreeMap<String, String> {
        let digest = compute_hmac(config.algorithm, &config.secret, body);
        let signature = match config.signature_encoding {
            SignatureEncoding::Hex => hex::encode(&digest),
            SignatureEncoding::Base64 => BASE64_STANDARD.encode(&digest),
        };
        let prefix = config.signature_prefix.clone().unwrap_or_default();
        let mut headers = BTreeMap::new();
        headers.insert(
            config.signature_header.clone(),
            format!("{prefix}{signature}"),
        );
        headers.insert(config.delivery_id_header.clone(), delivery_id.to_string());
        headers
    }

    async fn reset() {
        clear_webhook_intake_state();
        crate::event_log::reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn round_trip_accepts_then_dedupes() {
        reset().await;
        let body = br#"{"hello":"world"}"#.to_vec();
        let config = make_config(b"super-secret");
        let snapshot = register_webhook_intake(config.clone()).expect("register");

        let headers = signed_headers(&config, &body, "delivery-1");
        let outcome = feed_webhook_intake(
            snapshot.id.as_str(),
            build_request(headers.clone(), body.clone(), None, None),
        )
        .await
        .expect("feed");
        assert_eq!(outcome.status, "accepted");
        assert_eq!(outcome.delivery_id.as_deref(), Some("delivery-1"));
        assert!(outcome.topic_event_id.is_some());

        let dup = feed_webhook_intake(
            snapshot.id.as_str(),
            build_request(headers, body, None, None),
        )
        .await
        .expect("feed dup");
        assert_eq!(dup.status, "duplicate");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_bad_signature() {
        reset().await;
        let body = b"payload".to_vec();
        let config = make_config(b"correct-secret");
        let snapshot = register_webhook_intake(config.clone()).expect("register");

        let mut headers = signed_headers(&config, &body, "delivery-1");
        headers.insert(
            config.signature_header.clone(),
            "sha256=deadbeef".to_string(),
        );
        let outcome = feed_webhook_intake(
            snapshot.id.as_str(),
            build_request(headers, body, None, None),
        )
        .await
        .expect("feed");
        assert_eq!(outcome.status, "rejected");
        assert!(outcome
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("signature"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn isolates_two_intakes_on_different_paths() {
        reset().await;
        let body = b"shared-body".to_vec();
        let mut config_a = make_config(b"secret-a");
        config_a.path = Some("/hooks/a".to_string());
        config_a.topic = "tests.intake_a".to_string();
        config_a.delivery_id_header = "x-a-delivery".to_string();

        let mut config_b = make_config(b"secret-b");
        config_b.path = Some("/hooks/b".to_string());
        config_b.topic = "tests.intake_b".to_string();
        config_b.delivery_id_header = "x-b-delivery".to_string();

        let snap_a = register_webhook_intake(config_a.clone()).expect("register a");
        let snap_b = register_webhook_intake(config_b.clone()).expect("register b");

        // Same delivery id on both intakes must NOT collide.
        let headers_a = signed_headers(&config_a, &body, "shared-delivery");
        let outcome_a = feed_webhook_intake(
            snap_a.id.as_str(),
            build_request(headers_a, body.clone(), Some("/hooks/a".to_string()), None),
        )
        .await
        .expect("feed a");
        assert_eq!(outcome_a.status, "accepted");

        let headers_b = signed_headers(&config_b, &body, "shared-delivery");
        let outcome_b = feed_webhook_intake(
            snap_b.id.as_str(),
            build_request(headers_b, body, Some("/hooks/b".to_string()), None),
        )
        .await
        .expect("feed b");
        assert_eq!(outcome_b.status, "accepted");

        // And path mismatch is rejected.
        let body = b"more".to_vec();
        let headers = signed_headers(&config_a, &body, "another-delivery");
        let outcome = feed_webhook_intake(
            snap_a.id.as_str(),
            build_request(headers, body, Some("/hooks/wrong".to_string()), None),
        )
        .await
        .expect("feed mismatch");
        assert_eq!(outcome.status, "rejected");
        assert!(outcome.reason.unwrap().contains("path mismatch"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn supports_sha1_legacy() {
        reset().await;
        let body = b"legacy".to_vec();
        let mut config = make_config(b"legacy-secret");
        config.algorithm = HmacAlgorithm::Sha1;
        config.allow_legacy_sha1 = true;
        config.signature_prefix = Some("sha1=".to_string());
        let snapshot = register_webhook_intake(config.clone()).expect("register");

        let headers = signed_headers(&config, &body, "legacy-delivery");
        let outcome = feed_webhook_intake(
            snapshot.id.as_str(),
            build_request(headers, body, None, None),
        )
        .await
        .expect("feed");
        assert_eq!(outcome.status, "accepted");
    }

    #[test]
    fn hmac_algorithm_parser_gates_sha1() {
        assert_eq!(HmacAlgorithm::parse("sha256"), Some(HmacAlgorithm::Sha256));
        assert_eq!(HmacAlgorithm::parse("sha1"), None);
        assert_eq!(
            HmacAlgorithm::parse_with_legacy_sha1("sha1", true),
            Some(HmacAlgorithm::Sha1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_sha1_without_legacy_opt_in() {
        reset().await;
        let mut config = make_config(b"legacy-secret");
        config.algorithm = HmacAlgorithm::Sha1;
        config.signature_prefix = Some("sha1=".to_string());

        let error = register_webhook_intake(config).expect_err("sha1 requires opt-in");
        assert!(error.to_string().contains("set `allow_legacy_sha1: true`"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dedupe_survives_process_restart() {
        clear_webhook_intake_state();
        crate::event_log::reset_active_event_log();
        let tmp = tempfile::tempdir().expect("tempdir");
        // Install a file-backed event log so the inbox can rehydrate from disk.
        let log = Arc::new(AnyEventLog::File(
            FileEventLog::open(tmp.path().to_path_buf(), 32).expect("file log"),
        ));
        crate::event_log::install_active_event_log(log.clone());

        let body = br#"{"hello":"durable"}"#.to_vec();
        let mut config = make_config(b"durable-secret");
        // Pin the id so the post-restart re-register uses the same dedupe
        // namespace; without this each register call would mint a fresh id.
        config.id = Some("durable-intake".to_string());
        let snapshot = register_webhook_intake(config.clone()).expect("register");
        let headers = signed_headers(&config, &body, "durable-1");

        let first = feed_webhook_intake(
            snapshot.id.as_str(),
            build_request(headers.clone(), body.clone(), None, None),
        )
        .await
        .expect("first feed");
        assert_eq!(first.status, "accepted");

        // Simulate a process restart by clearing in-memory intake state and the
        // active event log handle, then reinstalling a fresh file-backed log on
        // the same directory.
        clear_webhook_intake_state();
        crate::event_log::reset_active_event_log();
        let log = Arc::new(AnyEventLog::File(
            FileEventLog::open(tmp.path().to_path_buf(), 32).expect("file log"),
        ));
        crate::event_log::install_active_event_log(log);
        let snapshot = register_webhook_intake(config.clone()).expect("re-register");

        let second = feed_webhook_intake(
            snapshot.id.as_str(),
            build_request(headers, body, None, None),
        )
        .await
        .expect("second feed");
        assert_eq!(second.status, "duplicate");

        // Use install_default_for_base_dir to keep parity with stdlib path
        // (no assertion — just ensures the helper still works).
        let _log = install_default_for_base_dir(tmp.path()).expect("install default");
    }
}
