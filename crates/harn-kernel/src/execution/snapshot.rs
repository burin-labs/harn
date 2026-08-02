use serde::{Deserialize, Serialize};

use super::{diagnostic, CapabilityResult, DataValue};
use crate::Diagnostic;

const SNAPSHOT_MAGIC: &[u8; 8] = b"HARNSP01";
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
pub(super) const SNAPSHOT_TAG_BYTES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ReplaySnapshot {
    pub(super) artifact_digest: [u8; 32],
    pub(super) grant_fingerprint: [u8; 32],
    pub(super) fuel_consumed: u64,
    pub(super) input: DataValue,
    pub(super) responses: Vec<CapabilityResult>,
    pub(super) pending_request: String,
}

pub(super) fn encode_snapshot(
    snapshot: &ReplaySnapshot,
    snapshot_key: &[u8; 32],
) -> Result<Vec<u8>, Diagnostic> {
    let payload = serde_json::to_vec(snapshot)
        .map_err(|error| diagnostic("snapshot_encode", error.to_string()))?;
    let tag = blake3::keyed_hash(snapshot_key, &payload);
    let mut bytes = Vec::with_capacity(12 + payload.len() + SNAPSHOT_TAG_BYTES);
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(tag.as_bytes());
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(diagnostic(
            "snapshot_too_large",
            "snapshot exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

pub(super) fn decode_snapshot(
    bytes: &[u8],
    snapshot_key: Option<&[u8; 32]>,
) -> Result<ReplaySnapshot, Diagnostic> {
    let Some(snapshot_key) = snapshot_key else {
        return Err(diagnostic(
            "snapshot_key_required",
            "resuming a suspended execution requires its host-owned snapshot key",
        ));
    };
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(diagnostic(
            "snapshot_too_large",
            "snapshot exceeds its byte limit",
        ));
    }
    if bytes.len() < 12 + SNAPSHOT_TAG_BYTES || &bytes[..8] != SNAPSHOT_MAGIC {
        return Err(diagnostic(
            "snapshot_malformed",
            "snapshot header is invalid",
        ));
    }
    let length = u32::from_be_bytes(bytes[8..12].try_into().expect("length checked")) as usize;
    if length != bytes.len() - 12 - SNAPSHOT_TAG_BYTES {
        return Err(diagnostic(
            "snapshot_malformed",
            "snapshot length does not match its header",
        ));
    }
    let payload_end = 12 + length;
    let payload = &bytes[12..payload_end];
    let expected = blake3::keyed_hash(snapshot_key, payload);
    if bytes[payload_end..] != expected.as_bytes()[..] {
        return Err(diagnostic(
            "snapshot_authentication",
            "snapshot authentication failed",
        ));
    }
    serde_json::from_slice(payload)
        .map_err(|error| diagnostic("snapshot_malformed", error.to_string()))
}
