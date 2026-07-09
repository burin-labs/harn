//! Ed25519 signing for session event chains.
//!
//! The store stamps each event with a sha256 `record_hash` linking
//! back to its predecessor's hash. The `SessionSigner` finalises a
//! session by emitting a `Receipt` event whose signature covers the
//! entire chain. The same key also signs individual events when the
//! caller opts in (`append_signed`) — useful for cross-tenant audit
//! trails where every event needs independent verification.
//!
//! Key material is deterministically derived from a 32-byte seed
//! supplied by the host. In production the seed comes from the
//! configured secret store ([`harn_vm::provenance::load_or_generate_agent_signing_key`]
//! is the long-form helper); for tests we accept a literal seed so
//! the verifier can be exercised without a real KMS.

use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use super::event::{canonical_event_bytes, canonical_json_bytes, EventSignature, StoredEvent};

pub const ALGORITHM: &str = "ed25519";

#[derive(Clone)]
pub struct SessionSigner {
    inner: std::sync::Arc<SigningKey>,
    key_id: String,
}

impl SessionSigner {
    /// Build a signer from a 32-byte seed. The verifying key id is
    /// computed as `"sha256:" + hex(sha256(verifying_key_bytes))[..32]`
    /// so the same seed always produces the same `signed_by.key_id`.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let key = SigningKey::from_bytes(&seed);
        let key_id = key_id_for(&key.verifying_key());
        Self {
            inner: std::sync::Arc::new(key),
            key_id,
        }
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.inner.verifying_key()
    }

    pub fn sign_event(&self, event: &StoredEvent) -> EventSignature {
        let bytes = canonical_event_bytes(event);
        sign_bytes(&self.inner, &self.key_id, &bytes)
    }

    /// Sign a finalisation receipt over the full event-root hash. The
    /// payload bytes are folded into the receipt event so a verifier
    /// can recompute them from the stored chain alone.
    pub fn sign_receipt(&self, receipt_root_hash: &str) -> EventSignature {
        let material = receipt_signing_material(receipt_root_hash);
        sign_bytes(&self.inner, &self.key_id, material.as_bytes())
    }
}

fn sign_bytes(key: &SigningKey, key_id: &str, bytes: &[u8]) -> EventSignature {
    let signature: Signature = key.sign(bytes);
    EventSignature {
        algorithm: ALGORITHM.to_string(),
        key_id: key_id.to_string(),
        signature: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
    }
}

fn receipt_signing_material(receipt_root_hash: &str) -> String {
    format!("harn.session.receipt.v1\nroot={receipt_root_hash}\n")
}

pub fn key_id_for(verifying_key: &VerifyingKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifying_key.as_bytes());
    let digest = hasher.finalize();
    format!("sha256:{}", &hex::encode(digest)[..32])
}

/// Compute the canonical record hash for an event with `prev_hash`
/// already populated. Result is `"sha256:<hex>"`.
pub fn compute_record_hash(event: &StoredEvent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_event_bytes(event));
    finalize_sha256(hasher)
}

/// Wrap a finalised SHA-256 in the canonical `"sha256:<hex>"` label
/// every chain primitive prints. Centralising the format keeps the
/// algorithm tag in one place so a future cutover to a different hash
/// doesn't fan out across the module.
fn finalize_sha256(hasher: Sha256) -> String {
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Verify a per-event signature.
#[derive(Debug)]
pub enum VerifyError {
    NotSigned,
    UnsupportedAlgorithm(String),
    DecodeError(String),
    InvalidShape(String),
    BadSignature,
    HashMismatch { stored: String, computed: String },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSigned => write!(f, "event is not signed"),
            Self::UnsupportedAlgorithm(algorithm) => {
                write!(f, "unsupported signature algorithm '{algorithm}'")
            }
            Self::DecodeError(message) => {
                write!(f, "signature base64 decode failed: {message}")
            }
            Self::InvalidShape(message) => write!(f, "signature shape invalid: {message}"),
            Self::BadSignature => write!(f, "signature did not verify against the key"),
            Self::HashMismatch { stored, computed } => {
                write!(
                    f,
                    "record_hash mismatch: stored '{stored}' vs computed '{computed}'"
                )
            }
        }
    }
}

impl std::error::Error for VerifyError {}

pub fn verify_event(event: &StoredEvent, verifying_key: &VerifyingKey) -> Result<(), VerifyError> {
    let computed = compute_record_hash(event);
    if computed != event.record_hash {
        return Err(VerifyError::HashMismatch {
            stored: event.record_hash.clone(),
            computed,
        });
    }
    let Some(signed_by) = event.signed_by.as_ref() else {
        return Err(VerifyError::NotSigned);
    };
    verify_signature(signed_by, verifying_key, &canonical_event_bytes(event))
}

pub fn verify_receipt_root(
    signed_by: &EventSignature,
    verifying_key: &VerifyingKey,
    receipt_root_hash: &str,
) -> Result<(), VerifyError> {
    let material = receipt_signing_material(receipt_root_hash);
    verify_signature(signed_by, verifying_key, material.as_bytes())
}

fn verify_signature(
    signed_by: &EventSignature,
    verifying_key: &VerifyingKey,
    bytes: &[u8],
) -> Result<(), VerifyError> {
    if signed_by.algorithm != ALGORITHM {
        return Err(VerifyError::UnsupportedAlgorithm(
            signed_by.algorithm.clone(),
        ));
    }
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed_by.signature)
        .map_err(|error| VerifyError::DecodeError(error.to_string()))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|error| VerifyError::InvalidShape(error.to_string()))?;
    verifying_key
        .verify(bytes, &signature)
        .map_err(|_| VerifyError::BadSignature)
}

/// Initial chain root before any events have been appended. The prefix
/// is versioned so a future schema change doesn't silently re-validate
/// against an old chain.
pub fn chain_root_init() -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"harn.session.chain.v2");
    finalize_sha256(hasher)
}

/// Fold a single event's `record_hash` into the running chain root.
/// Composing folds in sequence reproduces [`chain_root_hash`] without
/// re-hashing the entire history on every append.
pub fn chain_root_fold(prev_root: &str, record_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_root.as_bytes());
    hasher.update(b"\n");
    hasher.update(record_hash.as_bytes());
    finalize_sha256(hasher)
}

/// Build the chain root hash for a list of stored events by replaying
/// the fold from genesis. Used by `verify` and by snapshot/replay; the
/// hot append path uses [`chain_root_fold`] directly.
pub fn chain_root_hash(events: &[StoredEvent]) -> String {
    events.iter().fold(chain_root_init(), |root, event| {
        chain_root_fold(&root, &event.record_hash)
    })
}

/// Re-anchor a chain of events on a new owning session id. The
/// `session_id` field is rewritten on each event and `prev_hash` +
/// `record_hash` are recomputed sequentially, so the resulting chain
/// is bytewise-verifiable as a standalone session. Used by
/// [`crate::SessionStore::fork`] to give the child session
/// a chain that `verify` can attest without the parent.
pub fn re_anchor_events(events: &[StoredEvent], new_session_id: &str) -> Vec<StoredEvent> {
    let mut rewritten = Vec::with_capacity(events.len());
    let mut prev_hash: Option<String> = None;
    for event in events {
        let mut copied = event.clone();
        copied.session_id = new_session_id.to_string();
        copied.prev_hash = prev_hash.clone();
        copied.record_hash = compute_record_hash(&copied);
        // Per-event signatures are detached over the canonical bytes;
        // since both session_id and prev_hash changed, the parent's
        // signature no longer attests this event. Drop it — the
        // session-close path will mint a fresh receipt covering the
        // re-anchored chain.
        copied.signed_by = None;
        prev_hash = Some(copied.record_hash.clone());
        rewritten.push(copied);
    }
    rewritten
}

/// Helper for receipt payloads built outside the signer.
pub fn canonical_receipt_payload(
    session_id: &str,
    last_event_id: super::event::EventId,
    chain_root: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "harn.session.receipt.v1",
        "session_id": session_id,
        "last_event_id": last_event_id,
        "chain_root": chain_root,
    })
}

/// Canonicalise a [`serde_json::Value`] for use in tests that compare
/// hashing inputs. Public surface kept tight; mostly used by the
/// integration tests that round-trip events.
pub fn canonical_value_hash(value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json_bytes(value));
    finalize_sha256(hasher)
}
