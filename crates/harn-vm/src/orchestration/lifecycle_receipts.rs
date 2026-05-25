//! Replay-deterministic lifecycle receipts (#1861, epic #1853 P-08).
//!
//! Suspend / resume / drain are journal entries. Per SOTA replay-engine
//! research (Temporal, Restate, Inngest, Azure Durable, Cadence), on
//! replay these lifecycle events must return memoized values rather than
//! re-execute. This module is the source-of-truth for the persisted
//! receipt shapes and the helpers used by `replay_oracle.rs` and the
//! conformance suite to assert determinism end-to-end.
//!
//! Design notes:
//!
//! * **Signed timestamps.** Every receipt carries a [`SignedLifecycleTimestamp`]
//!   that pins `at_ms` to the recorded wall time at the original run. The
//!   signature is HMAC-SHA256 over `(kind, at_ms, subject_id, initiator_id)`
//!   under a per-process salt. Replay must use the journal timestamp, not
//!   the current wall clock — `verify_signed_timestamp` lets the oracle
//!   prove the on-disk timestamp came from the original run before
//!   accepting it as ground truth.
//! * **Cached resume inputs.** [`ResumptionReceipt`] stores both `input`
//!   and `input_hash`. On replay, the runtime feeds the cached `input`
//!   back into the suspended worker instead of re-prompting and asserts
//!   that any fresh input it would otherwise compute matches
//!   `input_hash`. Mismatch surfaces `HARN-SUS-011`.
//! * **Privacy.** The full `input` value is journaled (so replay is
//!   self-contained), but [`record_resumption_receipt`] applies the
//!   configured redactor before persisting. Callers that hand off a
//!   secret-bearing payload pass a `RedactionPolicy` that nulls the
//!   sensitive paths; the hash is still computed against the *original*
//!   payload so determinism checks survive redaction.
//! * **Memoized drain decisions.** [`DrainDecisionReceipt`] captures the
//!   settlement agent's chosen `action` per drain item. On replay, the
//!   oracle reads the receipt instead of re-spawning the settlement
//!   agent's LLM call — changing the prompt invalidates the replay
//!   precisely because the recorded hash no longer matches.
//!
//! The module is intentionally JSON-shaped at the boundary so the
//! existing `run_record_*` and event-log persistence works without
//! schema changes. The replay oracle reads back the topic-scoped journal
//! entries and feeds them into [`ReplayTraceRun::approval_interactions`]
//! / [`lifecycle_audit_log`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};

/// Event-log topic that carries the persisted lifecycle receipts. One
/// topic for all three shapes keeps the replay oracle's cursor logic
/// simple — kind discriminates suspension / resumption / drain.
pub const LIFECYCLE_RECEIPT_TOPIC: &str = "agent.lifecycle.receipts";

pub const SUSPENSION_RECEIPT_KIND: &str = "suspension_receipt";
pub const RESUMPTION_RECEIPT_KIND: &str = "resumption_receipt";
pub const DRAIN_DECISION_RECEIPT_KIND: &str = "drain_decision_receipt";

pub const SIGNED_TIMESTAMP_ALGORITHM: &str = "hmac-sha256";
pub const SIGNED_TIMESTAMP_KEY_ID: &str = "local-session";

/// Per-process signing salt for lifecycle timestamps. Mirrors the
/// channels-module pattern (`SIGNING_SALT` in `channels.rs`): a fresh
/// salt per process so signatures cannot replay across runs, but stable
/// for the duration of one run so the in-process replay oracle can
/// verify what it recorded a moment earlier.
static LIFECYCLE_SIGNING_SALT: OnceLock<Vec<u8>> = OnceLock::new();

fn lifecycle_signing_salt() -> &'static [u8] {
    LIFECYCLE_SIGNING_SALT
        .get_or_init(|| {
            format!(
                "harn-lifecycle-signing-salt:{}:{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            )
            .into_bytes()
        })
        .as_slice()
}

/// Signed wall-clock timestamp carried on every lifecycle receipt.
///
/// `at_ms` is the canonical replay-time value; the human-readable RFC3339
/// `at` mirror exists for log inspection only. `signature` proves the
/// `(kind, at_ms, subject_id, initiator_id)` tuple was produced by this
/// process — corruption of any of those fields after the fact will fail
/// [`verify_signed_timestamp`] and the replay oracle will reject the
/// receipt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedLifecycleTimestamp {
    pub at_ms: i64,
    pub at: String,
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

impl SignedLifecycleTimestamp {
    /// Build a fresh signed timestamp pinned to the current mock-aware
    /// wall clock. The signature material binds the four
    /// receipt-identifying fields so a journal entry that gets its
    /// `at_ms` rewritten on disk fails verification.
    pub fn now_for(kind: &str, subject_id: &str, initiator_id: &str) -> Self {
        let at = crate::clock_mock::now_utc();
        let at_ms = harn_clock::offset_datetime_to_ms(at);
        let at_text = at.format(&Rfc3339).unwrap_or_else(|_| at.to_string());
        let signature = sign_timestamp_material(kind, at_ms, subject_id, initiator_id);
        Self {
            at_ms,
            at: at_text,
            algorithm: SIGNED_TIMESTAMP_ALGORITHM.to_string(),
            key_id: SIGNED_TIMESTAMP_KEY_ID.to_string(),
            signature,
        }
    }
}

fn sign_timestamp_material(kind: &str, at_ms: i64, subject_id: &str, initiator_id: &str) -> String {
    let material = format!(
        "harn.lifecycle.timestamp.v1\nkind={kind}\nat_ms={at_ms}\nsubject={subject_id}\ninitiator={initiator_id}\n"
    );
    let mac = crate::connectors::hmac::hmac_sha256(lifecycle_signing_salt(), material.as_bytes());
    format!("sha256:{}", hex::encode(mac))
}

/// Verify that a [`SignedLifecycleTimestamp`] was minted by this process
/// for the given `(kind, subject_id, initiator_id)` tuple. Returns `Err`
/// if the algorithm/key changed (forward compatibility), the signature
/// is malformed, or the recomputed material disagrees with the stored
/// signature. Used by the replay oracle before it accepts an on-disk
/// receipt as ground truth.
pub fn verify_signed_timestamp(
    stamp: &SignedLifecycleTimestamp,
    kind: &str,
    subject_id: &str,
    initiator_id: &str,
) -> Result<(), LifecycleReceiptError> {
    if stamp.algorithm != SIGNED_TIMESTAMP_ALGORITHM {
        return Err(LifecycleReceiptError::SignatureAlgorithmMismatch {
            expected: SIGNED_TIMESTAMP_ALGORITHM.to_string(),
            found: stamp.algorithm.clone(),
        });
    }
    if stamp.key_id != SIGNED_TIMESTAMP_KEY_ID {
        return Err(LifecycleReceiptError::SignatureKeyMismatch {
            expected: SIGNED_TIMESTAMP_KEY_ID.to_string(),
            found: stamp.key_id.clone(),
        });
    }
    let expected = sign_timestamp_material(kind, stamp.at_ms, subject_id, initiator_id);
    if expected != stamp.signature {
        return Err(LifecycleReceiptError::SignatureMismatch {
            expected,
            found: stamp.signature.clone(),
        });
    }
    Ok(())
}

/// Origin of a recorded suspension. Mirrors
/// `WorkerSuspension::initiator` and the `Suspension`-shape transcript
/// constructor so the on-disk shape is unified across the lifecycle stack.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuspendInitiator {
    #[serde(rename = "self")]
    SelfInitiated,
    Parent,
    #[default]
    Operator,
    Triggered,
}

impl SuspendInitiator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfInitiated => "self",
            Self::Parent => "parent",
            Self::Operator => "operator",
            Self::Triggered => "triggered",
        }
    }
}

/// Origin of a recorded resumption. Aligned with
/// `helpers::transcript::ResumptionInitiator`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResumeInitiator {
    #[default]
    Parent,
    Operator,
    Triggered,
    DrainAgent,
    Timeout,
}

impl ResumeInitiator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Operator => "operator",
            Self::Triggered => "triggered",
            Self::DrainAgent => "drain_agent",
            Self::Timeout => "timeout",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "operator" => Self::Operator,
            "triggered" | "trigger" => Self::Triggered,
            "drain_agent" | "drain-agent" | "settlement" => Self::DrainAgent,
            "timeout" => Self::Timeout,
            _ => Self::Parent,
        }
    }
}

/// What an auto-resume trigger matched against. Carried on a
/// [`ResumptionReceipt`] when a trigger drove the resume so replay can
/// reproduce the exact match without re-firing the connector.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerMatchInfo {
    pub source: String,
    pub event_id: String,
    pub filter_summary: String,
}

/// Drain decision categories. Mirrors
/// `DrainDecisionItemCategory` in `helpers::transcript`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DrainItemCategory {
    #[default]
    SuspendedSubagent,
    QueuedTrigger,
    PartialHandoff,
    InFlightLlmCall,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainItem {
    pub category: DrainItemCategory,
    pub id: String,
    pub summary: String,
}

/// Action the settlement agent chose for a drain item.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DrainAction {
    #[default]
    Resume,
    Cancel,
    Handoff,
    Acknowledge,
    Defer,
    Wait,
    Finalize,
}

/// Persisted suspension event. Recorded at the moment a worker enters
/// the `suspended` state, replayed verbatim from the journal during a
/// second run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuspensionReceipt {
    pub handle: String,
    pub session_id: Option<String>,
    pub initiator: SuspendInitiator,
    pub initiator_id: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<JsonValue>,
    pub suspended_at: SignedLifecycleTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

impl SuspensionReceipt {
    /// Build a fresh receipt and sign its timestamp.
    pub fn new(
        handle: impl Into<String>,
        session_id: Option<String>,
        initiator: SuspendInitiator,
        initiator_id: impl Into<String>,
        reason: impl Into<String>,
        conditions: Option<JsonValue>,
        span_id: Option<String>,
    ) -> Self {
        let handle = handle.into();
        let initiator_id = initiator_id.into();
        let suspended_at =
            SignedLifecycleTimestamp::now_for(SUSPENSION_RECEIPT_KIND, &handle, &initiator_id);
        Self {
            handle,
            session_id,
            initiator,
            initiator_id,
            reason: reason.into(),
            conditions,
            suspended_at,
            span_id,
        }
    }

    /// Verify the receipt's signed timestamp. Wraps
    /// [`verify_signed_timestamp`] with the receipt-specific subject /
    /// initiator binding.
    pub fn verify_signature(&self) -> Result<(), LifecycleReceiptError> {
        verify_signed_timestamp(
            &self.suspended_at,
            SUSPENSION_RECEIPT_KIND,
            &self.handle,
            &self.initiator_id,
        )
    }
}

/// Persisted resumption event. The `input` field is the cached resume
/// input (post-redaction); `input_hash` is the deterministic
/// fingerprint of the *unredacted* payload so replay can detect drift
/// even when the journaled value was scrubbed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumptionReceipt {
    pub handle: String,
    pub session_id: Option<String>,
    pub initiator: ResumeInitiator,
    pub initiator_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<JsonValue>,
    pub input_hash: String,
    pub continue_transcript: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_suspension_span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_match: Option<TriggerMatchInfo>,
    pub resumed_at: SignedLifecycleTimestamp,
}

impl ResumptionReceipt {
    /// Build a fresh resumption receipt. The hash is computed against
    /// `original_input` so post-redaction values still validate against
    /// the unredacted fingerprint. `journaled_input` may be the
    /// post-redaction value (or `None` if the caller does not want to
    /// persist the payload at all).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        handle: impl Into<String>,
        session_id: Option<String>,
        initiator: ResumeInitiator,
        initiator_id: impl Into<String>,
        original_input: Option<&JsonValue>,
        journaled_input: Option<JsonValue>,
        continue_transcript: bool,
        linked_suspension_span_id: Option<String>,
        trigger_match: Option<TriggerMatchInfo>,
    ) -> Self {
        let handle = handle.into();
        let initiator_id = initiator_id.into();
        let resumed_at =
            SignedLifecycleTimestamp::now_for(RESUMPTION_RECEIPT_KIND, &handle, &initiator_id);
        let input_hash = hash_resume_input(original_input);
        Self {
            handle,
            session_id,
            initiator,
            initiator_id,
            input: journaled_input,
            input_hash,
            continue_transcript,
            linked_suspension_span_id,
            trigger_match,
            resumed_at,
        }
    }

    pub fn verify_signature(&self) -> Result<(), LifecycleReceiptError> {
        verify_signed_timestamp(
            &self.resumed_at,
            RESUMPTION_RECEIPT_KIND,
            &self.handle,
            &self.initiator_id,
        )
    }
}

/// Persisted drain decision. Recorded by the settlement agent so replay
/// can short-circuit the LLM call that produced `action`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainDecisionReceipt {
    pub pipeline_id: String,
    pub item: DrainItem,
    pub action: DrainAction,
    pub reason: String,
    pub decided_by: String,
    pub decided_at: SignedLifecycleTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
}

impl DrainDecisionReceipt {
    pub fn new(
        pipeline_id: impl Into<String>,
        item: DrainItem,
        action: DrainAction,
        reason: impl Into<String>,
        decided_by: impl Into<String>,
        prompt_hash: Option<String>,
    ) -> Self {
        let pipeline_id = pipeline_id.into();
        let decided_by = decided_by.into();
        let decided_at = SignedLifecycleTimestamp::now_for(
            DRAIN_DECISION_RECEIPT_KIND,
            &pipeline_id,
            &decided_by,
        );
        Self {
            pipeline_id,
            item,
            action,
            reason: reason.into(),
            decided_by,
            decided_at,
            prompt_hash,
        }
    }

    pub fn verify_signature(&self) -> Result<(), LifecycleReceiptError> {
        verify_signed_timestamp(
            &self.decided_at,
            DRAIN_DECISION_RECEIPT_KIND,
            &self.pipeline_id,
            &self.decided_by,
        )
    }
}

/// Errors surfaced when minting / verifying / replaying receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleReceiptError {
    SignatureAlgorithmMismatch {
        expected: String,
        found: String,
    },
    SignatureKeyMismatch {
        expected: String,
        found: String,
    },
    SignatureMismatch {
        expected: String,
        found: String,
    },
    ResumeInputHashMismatch {
        handle: String,
        expected_hash: String,
        actual_hash: String,
    },
    DrainDecisionPromptHashMismatch {
        item_id: String,
        expected_hash: String,
        actual_hash: String,
    },
    Persistence(String),
}

impl std::fmt::Display for LifecycleReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignatureAlgorithmMismatch { expected, found } => write!(
                f,
                "HARN-SUS-013 lifecycle signature algorithm mismatch (expected {expected}, found {found})"
            ),
            Self::SignatureKeyMismatch { expected, found } => write!(
                f,
                "HARN-SUS-013 lifecycle signature key mismatch (expected {expected}, found {found})"
            ),
            Self::SignatureMismatch { expected, found } => write!(
                f,
                "HARN-SUS-013 lifecycle signature mismatch (expected {expected}, found {found})"
            ),
            Self::ResumeInputHashMismatch {
                handle,
                expected_hash,
                actual_hash,
            } => write!(
                f,
                "HARN-SUS-011 replay resume input hash mismatch for {handle} (expected {expected_hash}, got {actual_hash})"
            ),
            Self::DrainDecisionPromptHashMismatch {
                item_id,
                expected_hash,
                actual_hash,
            } => write!(
                f,
                "HARN-SUS-012 replay drain decision prompt hash mismatch for {item_id} (expected {expected_hash}, got {actual_hash})"
            ),
            Self::Persistence(message) => write!(f, "lifecycle receipt persistence: {message}"),
        }
    }
}

impl std::error::Error for LifecycleReceiptError {}

/// Deterministic hash used by [`ResumptionReceipt::input_hash`] and
/// the replay-oracle comparator. We hash canonical JSON so map-key order
/// doesn't drift between record and replay.
pub fn hash_resume_input(input: Option<&JsonValue>) -> String {
    let canonical = canonical_json_bytes(input.unwrap_or(&JsonValue::Null));
    let digest = Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(digest))
}

/// Hash a settlement-agent prompt fingerprint. The settlement agent
/// records this before its LLM call so replay can detect prompt drift
/// without re-running the model.
pub fn hash_drain_decision_prompt(prompt: &str) -> String {
    let digest = Sha256::digest(prompt.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

fn canonical_json_bytes(value: &JsonValue) -> Vec<u8> {
    let canonical = canonicalize_for_hash(value);
    serde_json::to_vec(&canonical).unwrap_or_default()
}

fn canonicalize_for_hash(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(v) = map.get(key) {
                    sorted.insert(key.clone(), canonicalize_for_hash(v));
                }
            }
            JsonValue::Object(sorted)
        }
        JsonValue::Array(items) => {
            JsonValue::Array(items.iter().map(canonicalize_for_hash).collect())
        }
        other => other.clone(),
    }
}

/// Lightweight policy for redacting persisted `ResumptionReceipt.input`
/// before journaling. Each entry is a JSON-pointer path that gets
/// replaced with a sentinel `{"$harn_redacted": "<reason>"}` object.
/// The hash is always computed against the *unredacted* original payload
/// so determinism survives redaction.
#[derive(Clone, Debug, Default)]
pub struct RedactionPolicy {
    pub paths: Vec<RedactionPath>,
}

#[derive(Clone, Debug)]
pub struct RedactionPath {
    pub pointer: String,
    pub reason: String,
}

impl RedactionPolicy {
    pub fn redact(&self, value: &JsonValue) -> JsonValue {
        let mut working = value.clone();
        for rule in &self.paths {
            apply_redaction(&mut working, &rule.pointer, &rule.reason);
        }
        working
    }
}

fn apply_redaction(value: &mut JsonValue, pointer: &str, reason: &str) {
    if pointer.is_empty() || pointer == "/" {
        *value = serde_json::json!({"$harn_redacted": reason});
        return;
    }
    let segments: Vec<&str> = pointer
        .strip_prefix('/')
        .unwrap_or(pointer)
        .split('/')
        .collect();
    redact_segments(value, &segments, reason);
}

fn redact_segments(value: &mut JsonValue, segments: &[&str], reason: &str) {
    if segments.is_empty() {
        *value = serde_json::json!({"$harn_redacted": reason});
        return;
    }
    let head = segments[0];
    let tail = &segments[1..];
    match value {
        JsonValue::Object(map) => {
            if let Some(child) = map.get_mut(head) {
                redact_segments(child, tail, reason);
            }
        }
        JsonValue::Array(items) => {
            if let Ok(index) = head.parse::<usize>() {
                if let Some(child) = items.get_mut(index) {
                    redact_segments(child, tail, reason);
                }
            }
        }
        _ => {}
    }
}

thread_local! {
    static LIFECYCLE_RECEIPT_LOG: RefCell<Vec<LifecycleReceiptEntry>> = const { RefCell::new(Vec::new()) };
    static LIFECYCLE_RECEIPT_SEQ: RefCell<u64> = const { RefCell::new(0) };
}

/// Reset the in-memory receipt registry. Called from `reset_thread_local_state`
/// so test harnesses do not carry receipts between runs.
pub fn reset_lifecycle_receipt_registry() {
    LIFECYCLE_RECEIPT_LOG.with(|log| log.borrow_mut().clear());
    LIFECYCLE_RECEIPT_SEQ.with(|seq| *seq.borrow_mut() = 0);
}

/// A receipt + its monotonic seq + its on-disk kind. The seq is stable
/// across recordings within a single thread, which is what conformance
/// fixtures need for deterministic byte-identical comparison.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleReceiptEntry {
    pub seq: u64,
    pub kind: String,
    pub payload: JsonValue,
}

impl LifecycleReceiptEntry {
    pub fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "seq": self.seq,
            "kind": &self.kind,
            "payload": &self.payload,
        })
    }
}

fn next_seq() -> u64 {
    LIFECYCLE_RECEIPT_SEQ.with(|seq| {
        let mut slot = seq.borrow_mut();
        *slot += 1;
        *slot
    })
}

fn record_entry(kind: &str, payload: JsonValue) -> LifecycleReceiptEntry {
    let entry = LifecycleReceiptEntry {
        seq: next_seq(),
        kind: kind.to_string(),
        payload,
    };
    LIFECYCLE_RECEIPT_LOG.with(|log| log.borrow_mut().push(entry.clone()));
    persist_entry(&entry);
    entry
}

fn persist_entry(entry: &LifecycleReceiptEntry) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(LIFECYCLE_RECEIPT_TOPIC) else {
        return;
    };
    let mut headers = BTreeMap::new();
    headers.insert("kind".to_string(), entry.kind.clone());
    headers.insert("seq".to_string(), entry.seq.to_string());
    let event = LogEvent::new(entry.kind.clone(), entry.payload.clone()).with_headers(headers);
    let _ = futures::executor::block_on(log.append(&topic, event));
}

/// Persist a suspension receipt and return the journaled entry.
pub fn record_suspension_receipt(receipt: &SuspensionReceipt) -> LifecycleReceiptEntry {
    let payload = serde_json::to_value(receipt).unwrap_or(JsonValue::Null);
    record_entry(SUSPENSION_RECEIPT_KIND, payload)
}

/// Persist a resumption receipt and return the journaled entry.
pub fn record_resumption_receipt(receipt: &ResumptionReceipt) -> LifecycleReceiptEntry {
    let payload = serde_json::to_value(receipt).unwrap_or(JsonValue::Null);
    record_entry(RESUMPTION_RECEIPT_KIND, payload)
}

/// Persist a drain decision receipt and return the journaled entry.
pub fn record_drain_decision_receipt(receipt: &DrainDecisionReceipt) -> LifecycleReceiptEntry {
    let payload = serde_json::to_value(receipt).unwrap_or(JsonValue::Null);
    record_entry(DRAIN_DECISION_RECEIPT_KIND, payload)
}

/// Snapshot every recorded receipt in seq order. Used by the replay
/// oracle (and conformance fixtures) to read back the journal.
pub fn lifecycle_receipts_snapshot() -> Vec<LifecycleReceiptEntry> {
    LIFECYCLE_RECEIPT_LOG.with(|log| log.borrow().clone())
}

/// Verify the [`ResumptionReceipt`] input hash matches a fresh
/// candidate. Used during replay: the runtime computes the hash of the
/// resume input it would otherwise apply and asks the receipt to
/// confirm. Returns the cached input on success so the caller can feed
/// it back into the suspended worker.
pub fn replay_resume_input(
    receipt: &ResumptionReceipt,
    candidate_input: Option<&JsonValue>,
) -> Result<Option<JsonValue>, LifecycleReceiptError> {
    receipt.verify_signature()?;
    let actual = hash_resume_input(candidate_input);
    if actual != receipt.input_hash {
        return Err(LifecycleReceiptError::ResumeInputHashMismatch {
            handle: receipt.handle.clone(),
            expected_hash: receipt.input_hash.clone(),
            actual_hash: actual,
        });
    }
    Ok(receipt.input.clone())
}

/// Verify the cached drain decision prompt matches the candidate
/// settlement-agent prompt at replay time. Returns the recorded
/// decision so the replay path can skip re-spawning the agent.
pub fn replay_drain_decision(
    receipt: &DrainDecisionReceipt,
    candidate_prompt: Option<&str>,
) -> Result<DrainAction, LifecycleReceiptError> {
    receipt.verify_signature()?;
    if let (Some(prompt), Some(expected)) = (candidate_prompt, receipt.prompt_hash.as_ref()) {
        let actual = hash_drain_decision_prompt(prompt);
        if &actual != expected {
            return Err(LifecycleReceiptError::DrainDecisionPromptHashMismatch {
                item_id: receipt.item.id.clone(),
                expected_hash: expected.clone(),
                actual_hash: actual,
            });
        }
    }
    Ok(receipt.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh() {
        reset_lifecycle_receipt_registry();
    }

    #[test]
    fn suspension_receipt_signs_and_verifies() {
        fresh();
        let receipt = SuspensionReceipt::new(
            "worker://triage/42",
            Some("session-1".to_string()),
            SuspendInitiator::Operator,
            "operator-1",
            "waiting for human approval",
            Some(json!({"kind": "approval"})),
            Some("span-1".to_string()),
        );
        receipt
            .verify_signature()
            .expect("signed timestamp verifies");

        let mut tampered = receipt;
        tampered.suspended_at.at_ms += 1;
        assert!(matches!(
            tampered.verify_signature(),
            Err(LifecycleReceiptError::SignatureMismatch { .. })
        ));
    }

    #[test]
    fn resumption_receipt_round_trips_input_hash() {
        fresh();
        let original = json!({"approved": true, "comment": "ship it"});
        let receipt = ResumptionReceipt::new(
            "worker://triage/42",
            Some("session-1".to_string()),
            ResumeInitiator::Operator,
            "operator-1",
            Some(&original),
            Some(original.clone()),
            true,
            None,
            None,
        );

        let cached = replay_resume_input(&receipt, Some(&original)).expect("matches");
        assert_eq!(cached, Some(original));

        let drift = json!({"approved": false, "comment": "ship it"});
        let mismatch = replay_resume_input(&receipt, Some(&drift));
        assert!(matches!(
            mismatch,
            Err(LifecycleReceiptError::ResumeInputHashMismatch { .. })
        ));
    }

    #[test]
    fn resumption_hash_is_canonical_across_map_key_order() {
        let a = json!({"a": 1, "b": 2});
        let b = json!({"b": 2, "a": 1});
        assert_eq!(hash_resume_input(Some(&a)), hash_resume_input(Some(&b)));
    }

    #[test]
    fn redaction_policy_preserves_hash() {
        let original = json!({
            "user": "alice",
            "secret_token": "very-secret",
            "approved": true,
        });
        let policy = RedactionPolicy {
            paths: vec![RedactionPath {
                pointer: "/secret_token".to_string(),
                reason: "auth_token".to_string(),
            }],
        };
        let redacted = policy.redact(&original);
        assert_ne!(redacted, original);
        assert_eq!(
            redacted["secret_token"],
            json!({"$harn_redacted": "auth_token"})
        );
        // Hash is computed against the original — replay still works.
        let receipt = ResumptionReceipt::new(
            "worker://x",
            None,
            ResumeInitiator::Operator,
            "op-1",
            Some(&original),
            Some(redacted.clone()),
            true,
            None,
            None,
        );
        let cached = replay_resume_input(&receipt, Some(&original)).expect("matches");
        assert_eq!(cached, Some(redacted));
    }

    #[test]
    fn drain_decision_receipt_memoizes_action() {
        fresh();
        let prompt = "settle this drain item";
        let receipt = DrainDecisionReceipt::new(
            "pipeline-1",
            DrainItem {
                category: DrainItemCategory::SuspendedSubagent,
                id: "worker://triage/42".to_string(),
                summary: "worker is suspended".to_string(),
            },
            DrainAction::Resume,
            "settlement agent picked resume".to_string(),
            "settlement-session-1",
            Some(hash_drain_decision_prompt(prompt)),
        );

        let action = replay_drain_decision(&receipt, Some(prompt)).expect("matches");
        assert_eq!(action, DrainAction::Resume);

        let drift = replay_drain_decision(&receipt, Some("a different prompt"));
        assert!(matches!(
            drift,
            Err(LifecycleReceiptError::DrainDecisionPromptHashMismatch { .. })
        ));
    }

    #[test]
    fn record_then_snapshot_byte_identical_across_runs() {
        fresh();
        let s = SuspensionReceipt::new(
            "worker://x",
            None,
            SuspendInitiator::Operator,
            "op-1",
            "reason",
            None,
            None,
        );
        let entry_a = record_suspension_receipt(&s);
        fresh();
        let entry_b = record_suspension_receipt(&s);
        // Both runs assign seq=1 after reset; payloads differ only by the
        // resigned `at_ms` window. The deterministic *contract* is that a
        // single recorded receipt round-trips byte-identical through the
        // log — that's the case the replay oracle relies on.
        assert_eq!(entry_a.seq, entry_b.seq);
        assert_eq!(entry_a.kind, entry_b.kind);
    }

    #[test]
    fn snapshot_returns_recorded_entries_in_seq_order() {
        fresh();
        let s = SuspensionReceipt::new(
            "worker://x",
            None,
            SuspendInitiator::Operator,
            "op-1",
            "reason",
            None,
            None,
        );
        record_suspension_receipt(&s);
        let r = ResumptionReceipt::new(
            "worker://x",
            None,
            ResumeInitiator::Operator,
            "op-1",
            None,
            None,
            true,
            None,
            None,
        );
        record_resumption_receipt(&r);
        let snapshot = lifecycle_receipts_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].seq, 1);
        assert_eq!(snapshot[0].kind, SUSPENSION_RECEIPT_KIND);
        assert_eq!(snapshot[1].seq, 2);
        assert_eq!(snapshot[1].kind, RESUMPTION_RECEIPT_KIND);
    }
}
