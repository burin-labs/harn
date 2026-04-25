//! Harn Flow `Atom` primitive.
//!
//! An `Atom` is the smallest invertible, CRDT-clean change in Harn Flow.
//! Atoms are content-addressed: their `AtomId` is the SHA-256 of a canonical
//! binary encoding of every field except the id and signature themselves.
//! Every atom carries signed provenance (`(principal, persona, ...)`) and may
//! point at the atom it constructively reverses via `inverse_of`.
//!
//! Two encodings are supported and round-trip 1:1 with the in-memory struct:
//! * JSON, via `serde` — human-readable interchange / event-log payloads.
//! * Canonical binary — deterministic length-prefixed bytes used for hashing,
//!   signing, and on-disk storage.
//!
//! See issue #573 ("[flow] Atom type + signed-provenance schema") and parent
//! epic #571.

use std::fmt;

use base64::Engine as _;
use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const ATOM_BINARY_MAGIC: &[u8; 4] = b"FATM";
const ATOM_BINARY_VERSION: u8 = 1;
const ATOM_ID_BYTES: usize = 32;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;

/// Errors produced when constructing, encoding, decoding, signing, or
/// verifying an `Atom`.
#[derive(Debug)]
pub enum AtomError {
    /// JSON (de)serialization failure.
    Json(String),
    /// Canonical binary decode failure (truncation, bad tag, oversize length).
    Binary(String),
    /// Atom id did not match the recomputed content hash.
    ContentHashMismatch { expected: AtomId, actual: AtomId },
    /// `verify` failed because the principal or persona signature is invalid.
    InvalidSignature(&'static str),
    /// Apply/invert error (offset out of range or content mismatch).
    Apply(String),
    /// Misc validation error (negative timestamp, malformed key bytes, …).
    Invalid(String),
}

impl fmt::Display for AtomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AtomError::Json(message) => write!(f, "atom json error: {message}"),
            AtomError::Binary(message) => write!(f, "atom binary error: {message}"),
            AtomError::ContentHashMismatch { expected, actual } => write!(
                f,
                "atom id mismatch: expected {expected}, recomputed {actual}",
            ),
            AtomError::InvalidSignature(role) => write!(f, "{role} signature failed verification"),
            AtomError::Apply(message) => write!(f, "atom apply/invert error: {message}"),
            AtomError::Invalid(message) => write!(f, "atom invalid: {message}"),
        }
    }
}

impl std::error::Error for AtomError {}

/// 32-byte SHA-256 content address of an `Atom`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AtomId(pub [u8; ATOM_ID_BYTES]);

impl AtomId {
    /// Produce a hex-encoded representation suitable for logs and JSON.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-character hex string into an `AtomId`.
    pub fn from_hex(raw: &str) -> Result<Self, AtomError> {
        let bytes = hex::decode(raw)
            .map_err(|error| AtomError::Invalid(format!("invalid AtomId hex: {error}")))?;
        if bytes.len() != ATOM_ID_BYTES {
            return Err(AtomError::Invalid(format!(
                "AtomId must be {ATOM_ID_BYTES} bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; ATOM_ID_BYTES];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

impl fmt::Debug for AtomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AtomId({})", self.to_hex())
    }
}

impl fmt::Display for AtomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl Serialize for AtomId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for AtomId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        AtomId::from_hex(&raw).map_err(serde::de::Error::custom)
    }
}

/// A single text edit. Atoms hold an ordered list of these.
///
/// `Delete` carries the bytes it removes so the inverse operation
/// (`Insert` of the same content at the same offset) reconstructs the
/// pre-image without consulting the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextOp {
    /// Insert `content` at byte `offset`.
    Insert { offset: u64, content: String },
    /// Delete `content` at byte `offset`. `content` is the removed bytes.
    Delete { offset: u64, content: String },
}

impl TextOp {
    /// Return the constructive inverse: `Insert` ↔ `Delete` with the same
    /// offset and content.
    pub fn invert(&self) -> TextOp {
        match self {
            TextOp::Insert { offset, content } => TextOp::Delete {
                offset: *offset,
                content: content.clone(),
            },
            TextOp::Delete { offset, content } => TextOp::Insert {
                offset: *offset,
                content: content.clone(),
            },
        }
    }

    /// Apply this op to `document` (as a UTF-8 byte buffer). Returns an error
    /// if the offset is out of range or if a `Delete`'s recorded content does
    /// not match what is in the document at that offset.
    pub fn apply(&self, document: &mut Vec<u8>) -> Result<(), AtomError> {
        match self {
            TextOp::Insert { offset, content } => {
                let offset_usize = usize::try_from(*offset).map_err(|_| {
                    AtomError::Apply(format!("insert offset {offset} exceeds usize::MAX"))
                })?;
                if offset_usize > document.len() {
                    return Err(AtomError::Apply(format!(
                        "insert offset {offset_usize} > document length {}",
                        document.len()
                    )));
                }
                document.splice(offset_usize..offset_usize, content.bytes());
                Ok(())
            }
            TextOp::Delete { offset, content } => {
                let offset_usize = usize::try_from(*offset).map_err(|_| {
                    AtomError::Apply(format!("delete offset {offset} exceeds usize::MAX"))
                })?;
                let end = offset_usize.checked_add(content.len()).ok_or_else(|| {
                    AtomError::Apply(format!(
                        "delete range overflows: offset {offset_usize} + len {}",
                        content.len()
                    ))
                })?;
                if end > document.len() {
                    return Err(AtomError::Apply(format!(
                        "delete range {offset_usize}..{end} exceeds document length {}",
                        document.len()
                    )));
                }
                if &document[offset_usize..end] != content.as_bytes() {
                    return Err(AtomError::Apply(format!(
                        "delete content mismatch at offset {offset_usize}",
                    )));
                }
                document.drain(offset_usize..end);
                Ok(())
            }
        }
    }
}

/// Signed provenance of an atom: who emitted it, in what context, and when.
///
/// Field shapes intentionally mirror the strings used elsewhere in the runtime
/// (trust graph, observability spans, persona ledger) so atoms can be joined
/// against existing event streams without translation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Stable principal id (e.g. user account, service identity).
    pub principal: String,
    /// Persona name acting on behalf of the principal (e.g. `ship-captain`).
    pub persona: String,
    /// Run id of the agent invocation that produced this atom.
    pub agent_run_id: String,
    /// Tool-call id within that run, if the atom came from a specific call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Trace id binding this atom to a distributed trace.
    pub trace_id: String,
    /// Reference to the transcript that contextualizes this change.
    pub transcript_ref: String,
    /// Wall-clock timestamp the atom was created.
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

impl Provenance {
    /// Convenience constructor with a default `OffsetDateTime::now_utc()`
    /// timestamp.
    pub fn new(
        principal: impl Into<String>,
        persona: impl Into<String>,
        agent_run_id: impl Into<String>,
        trace_id: impl Into<String>,
        transcript_ref: impl Into<String>,
    ) -> Self {
        Self {
            principal: principal.into(),
            persona: persona.into(),
            agent_run_id: agent_run_id.into(),
            tool_call_id: None,
            trace_id: trace_id.into(),
            transcript_ref: transcript_ref.into(),
            timestamp: OffsetDateTime::now_utc(),
        }
    }
}

/// Detached Ed25519 signatures over the `AtomId`.
///
/// Both keys sign the same payload (the 32-byte AtomId). The principal key
/// represents the trust-graph identity that is ultimately accountable for the
/// change; the persona key represents the agent persona that emitted it.
/// Verification requires *both* signatures to be valid, which lets the trust
/// graph attribute revocations (per-persona rotation) without invalidating
/// atoms whose principal is still trusted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AtomSignature {
    pub principal_key: [u8; ED25519_PUBLIC_KEY_BYTES],
    pub principal_sig: [u8; ED25519_SIGNATURE_BYTES],
    pub persona_key: [u8; ED25519_PUBLIC_KEY_BYTES],
    pub persona_sig: [u8; ED25519_SIGNATURE_BYTES],
}

impl fmt::Debug for AtomSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomSignature")
            .field("principal_key", &hex::encode(self.principal_key))
            .field("persona_key", &hex::encode(self.persona_key))
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct AtomSignatureWire {
    principal_key: String,
    principal_sig: String,
    persona_key: String,
    persona_sig: String,
}

impl Serialize for AtomSignature {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let b64 = base64::engine::general_purpose::STANDARD;
        AtomSignatureWire {
            principal_key: b64.encode(self.principal_key),
            principal_sig: b64.encode(self.principal_sig),
            persona_key: b64.encode(self.persona_key),
            persona_sig: b64.encode(self.persona_sig),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AtomSignature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = AtomSignatureWire::deserialize(deserializer)?;
        let b64 = base64::engine::general_purpose::STANDARD;
        fn copy_into<const N: usize, E: serde::de::Error>(
            label: &str,
            raw: Vec<u8>,
        ) -> Result<[u8; N], E> {
            if raw.len() != N {
                return Err(serde::de::Error::custom(format!(
                    "{label} must be {N} bytes, got {}",
                    raw.len()
                )));
            }
            let mut out = [0u8; N];
            out.copy_from_slice(&raw);
            Ok(out)
        }
        let principal_key_bytes = b64
            .decode(wire.principal_key.as_bytes())
            .map_err(serde::de::Error::custom)?;
        let principal_sig_bytes = b64
            .decode(wire.principal_sig.as_bytes())
            .map_err(serde::de::Error::custom)?;
        let persona_key_bytes = b64
            .decode(wire.persona_key.as_bytes())
            .map_err(serde::de::Error::custom)?;
        let persona_sig_bytes = b64
            .decode(wire.persona_sig.as_bytes())
            .map_err(serde::de::Error::custom)?;
        Ok(AtomSignature {
            principal_key: copy_into::<ED25519_PUBLIC_KEY_BYTES, D::Error>(
                "principal_key",
                principal_key_bytes,
            )?,
            principal_sig: copy_into::<ED25519_SIGNATURE_BYTES, D::Error>(
                "principal_sig",
                principal_sig_bytes,
            )?,
            persona_key: copy_into::<ED25519_PUBLIC_KEY_BYTES, D::Error>(
                "persona_key",
                persona_key_bytes,
            )?,
            persona_sig: copy_into::<ED25519_SIGNATURE_BYTES, D::Error>(
                "persona_sig",
                persona_sig_bytes,
            )?,
        })
    }
}

/// The core flow primitive.
///
/// `id` is derived from the rest of the content; constructors and decoders
/// recompute and validate it. Two atoms that encode the same ops, parents,
/// provenance, and inverse_of always have the same id, regardless of which
/// encoding they were materialized from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Atom {
    pub id: AtomId,
    pub ops: Vec<TextOp>,
    pub parents: Vec<AtomId>,
    pub provenance: Provenance,
    pub signature: AtomSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse_of: Option<AtomId>,
}

impl Atom {
    /// Sign and assemble an atom from its content.
    ///
    /// Steps: encode the body canonically, derive `AtomId` as `SHA-256(body)`,
    /// sign that 32-byte id with both keys, return the assembled atom.
    pub fn sign(
        ops: Vec<TextOp>,
        parents: Vec<AtomId>,
        provenance: Provenance,
        inverse_of: Option<AtomId>,
        principal_key: &SigningKey,
        persona_key: &SigningKey,
    ) -> Result<Self, AtomError> {
        let body_bytes = encode_body_canonical(&ops, &parents, &provenance, &inverse_of)?;
        let id = AtomId(Sha256::digest(&body_bytes).into());
        let principal_sig = principal_key.sign(&id.0);
        let persona_sig = persona_key.sign(&id.0);
        Ok(Atom {
            id,
            ops,
            parents,
            provenance,
            inverse_of,
            signature: AtomSignature {
                principal_key: principal_key.verifying_key().to_bytes(),
                principal_sig: principal_sig.to_bytes(),
                persona_key: persona_key.verifying_key().to_bytes(),
                persona_sig: persona_sig.to_bytes(),
            },
        })
    }

    /// Build the inverse of `target`. The new atom's ops are the reverse
    /// list of `target.ops`, each individually inverted, with `inverse_of`
    /// pointing at `target.id`. Useful for "undo" — the user-facing model
    /// stacks an inverse atom rather than mutating history.
    pub fn invert(
        target: &Atom,
        provenance: Provenance,
        principal_key: &SigningKey,
        persona_key: &SigningKey,
    ) -> Result<Self, AtomError> {
        let ops: Vec<TextOp> = target.ops.iter().rev().map(TextOp::invert).collect();
        Atom::sign(
            ops,
            vec![target.id],
            provenance,
            Some(target.id),
            principal_key,
            persona_key,
        )
    }

    /// Recompute `id` from the body and check that it matches the stored
    /// `id`. Returns `Err(ContentHashMismatch)` on drift.
    pub fn verify_content_hash(&self) -> Result<(), AtomError> {
        let body_bytes =
            encode_body_canonical(&self.ops, &self.parents, &self.provenance, &self.inverse_of)?;
        let recomputed = AtomId(Sha256::digest(&body_bytes).into());
        if recomputed != self.id {
            return Err(AtomError::ContentHashMismatch {
                expected: self.id,
                actual: recomputed,
            });
        }
        Ok(())
    }

    /// Verify both signatures against the atom's id. Does *not* consult the
    /// trust graph — that's the caller's responsibility (the trust graph
    /// decides whether the keys themselves are currently trusted).
    pub fn verify_signatures(&self) -> Result<(), AtomError> {
        let signature_payload = self.id.0;
        let principal_key = VerifyingKey::from_bytes(&self.signature.principal_key)
            .map_err(|error| AtomError::Invalid(format!("principal key: {error}")))?;
        let persona_key = VerifyingKey::from_bytes(&self.signature.persona_key)
            .map_err(|error| AtomError::Invalid(format!("persona key: {error}")))?;
        let principal_sig = Ed25519Signature::from_bytes(&self.signature.principal_sig);
        let persona_sig = Ed25519Signature::from_bytes(&self.signature.persona_sig);
        principal_key
            .verify(&signature_payload, &principal_sig)
            .map_err(|_| AtomError::InvalidSignature("principal"))?;
        persona_key
            .verify(&signature_payload, &persona_sig)
            .map_err(|_| AtomError::InvalidSignature("persona"))?;
        Ok(())
    }

    /// Combined `verify_content_hash` + `verify_signatures`. Most callers
    /// want this.
    pub fn verify(&self) -> Result<(), AtomError> {
        self.verify_content_hash()?;
        self.verify_signatures()
    }

    /// Apply this atom's ops to `document` in order. Returns `Err` if any
    /// op fails (offset out of range or `Delete` content mismatch).
    pub fn apply(&self, document: &mut Vec<u8>) -> Result<(), AtomError> {
        for op in &self.ops {
            op.apply(document)?;
        }
        Ok(())
    }

    /// Encode as JSON. Round-trips with [`Atom::from_json_slice`].
    pub fn to_json(&self) -> Result<String, AtomError> {
        serde_json::to_string(self).map_err(|error| AtomError::Json(error.to_string()))
    }

    /// Decode from JSON, then verify the content hash (not signatures).
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, AtomError> {
        let atom: Atom =
            serde_json::from_slice(bytes).map_err(|error| AtomError::Json(error.to_string()))?;
        atom.verify_content_hash()?;
        Ok(atom)
    }

    /// Encode as canonical binary bytes. Round-trips with
    /// [`Atom::from_binary_slice`] and is byte-stable across processes.
    pub fn to_binary(&self) -> Result<Vec<u8>, AtomError> {
        encode_atom_binary(self)
    }

    /// Decode from canonical binary bytes, then verify the content hash
    /// (not signatures).
    pub fn from_binary_slice(bytes: &[u8]) -> Result<Self, AtomError> {
        let atom = decode_atom_binary(bytes)?;
        atom.verify_content_hash()?;
        Ok(atom)
    }
}

// ---------------------------------------------------------------------------
// Canonical binary encoding (deterministic, version-tagged).
// ---------------------------------------------------------------------------

fn encode_body_canonical(
    ops: &[TextOp],
    parents: &[AtomId],
    provenance: &Provenance,
    inverse_of: &Option<AtomId>,
) -> Result<Vec<u8>, AtomError> {
    let mut out = Vec::new();
    out.extend_from_slice(ATOM_BINARY_MAGIC);
    out.push(ATOM_BINARY_VERSION);
    write_ops(&mut out, ops);
    write_parents(&mut out, parents);
    write_provenance(&mut out, provenance)?;
    write_optional_atom_id(&mut out, inverse_of);
    Ok(out)
}

fn encode_atom_binary(atom: &Atom) -> Result<Vec<u8>, AtomError> {
    let mut out =
        encode_body_canonical(&atom.ops, &atom.parents, &atom.provenance, &atom.inverse_of)?;
    out.extend_from_slice(&atom.id.0);
    out.extend_from_slice(&atom.signature.principal_key);
    out.extend_from_slice(&atom.signature.principal_sig);
    out.extend_from_slice(&atom.signature.persona_key);
    out.extend_from_slice(&atom.signature.persona_sig);
    Ok(out)
}

fn write_ops(out: &mut Vec<u8>, ops: &[TextOp]) {
    write_u32(out, ops.len() as u32);
    for op in ops {
        match op {
            TextOp::Insert { offset, content } => {
                out.push(0);
                write_u64(out, *offset);
                write_bytes(out, content.as_bytes());
            }
            TextOp::Delete { offset, content } => {
                out.push(1);
                write_u64(out, *offset);
                write_bytes(out, content.as_bytes());
            }
        }
    }
}

fn write_parents(out: &mut Vec<u8>, parents: &[AtomId]) {
    write_u32(out, parents.len() as u32);
    for parent in parents {
        out.extend_from_slice(&parent.0);
    }
}

fn write_provenance(out: &mut Vec<u8>, provenance: &Provenance) -> Result<(), AtomError> {
    write_str(out, &provenance.principal);
    write_str(out, &provenance.persona);
    write_str(out, &provenance.agent_run_id);
    match &provenance.tool_call_id {
        Some(id) => {
            out.push(1);
            write_str(out, id);
        }
        None => out.push(0),
    }
    write_str(out, &provenance.trace_id);
    write_str(out, &provenance.transcript_ref);
    let formatted = provenance
        .timestamp
        .format(&Rfc3339)
        .map_err(|error| AtomError::Invalid(format!("timestamp format: {error}")))?;
    write_str(out, &formatted);
    Ok(())
}

fn write_optional_atom_id(out: &mut Vec<u8>, value: &Option<AtomId>) {
    match value {
        Some(id) => {
            out.push(1);
            out.extend_from_slice(&id.0);
        }
        None => out.push(0),
    }
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

fn write_str(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

// ---------------------------------------------------------------------------
// Canonical binary decoding.
// ---------------------------------------------------------------------------

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], AtomError> {
        if self.remaining() < n {
            return Err(AtomError::Binary(format!(
                "truncated: need {n} bytes, have {}",
                self.remaining()
            )));
        }
        let slice = &self.bytes[self.offset..self.offset + n];
        self.offset += n;
        Ok(slice)
    }

    fn take_u8(&mut self) -> Result<u8, AtomError> {
        Ok(self.take(1)?[0])
    }

    fn take_u32(&mut self) -> Result<u32, AtomError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn take_u64(&mut self) -> Result<u64, AtomError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn take_bytes(&mut self) -> Result<Vec<u8>, AtomError> {
        let len = self.take_u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn take_string(&mut self) -> Result<String, AtomError> {
        let bytes = self.take_bytes()?;
        String::from_utf8(bytes).map_err(|error| AtomError::Binary(format!("utf8: {error}")))
    }
}

fn decode_atom_binary(bytes: &[u8]) -> Result<Atom, AtomError> {
    let mut cursor = Cursor::new(bytes);
    let magic = cursor.take(ATOM_BINARY_MAGIC.len())?;
    if magic != ATOM_BINARY_MAGIC {
        return Err(AtomError::Binary("magic mismatch".to_string()));
    }
    let version = cursor.take_u8()?;
    if version != ATOM_BINARY_VERSION {
        return Err(AtomError::Binary(format!(
            "unsupported version {version}, expected {ATOM_BINARY_VERSION}"
        )));
    }

    let ops_len = cursor.take_u32()? as usize;
    let mut ops = Vec::with_capacity(ops_len);
    for _ in 0..ops_len {
        let tag = cursor.take_u8()?;
        let offset = cursor.take_u64()?;
        let content = cursor.take_string()?;
        ops.push(match tag {
            0 => TextOp::Insert { offset, content },
            1 => TextOp::Delete { offset, content },
            other => return Err(AtomError::Binary(format!("unknown op tag {other}"))),
        });
    }

    let parents_len = cursor.take_u32()? as usize;
    let mut parents = Vec::with_capacity(parents_len);
    for _ in 0..parents_len {
        let parent_bytes = cursor.take(ATOM_ID_BYTES)?;
        let mut id = [0u8; ATOM_ID_BYTES];
        id.copy_from_slice(parent_bytes);
        parents.push(AtomId(id));
    }

    let principal = cursor.take_string()?;
    let persona = cursor.take_string()?;
    let agent_run_id = cursor.take_string()?;
    let tool_call_id = match cursor.take_u8()? {
        0 => None,
        1 => Some(cursor.take_string()?),
        other => {
            return Err(AtomError::Binary(format!(
                "invalid tool_call_id tag {other}"
            )))
        }
    };
    let trace_id = cursor.take_string()?;
    let transcript_ref = cursor.take_string()?;
    let timestamp_str = cursor.take_string()?;
    let timestamp = OffsetDateTime::parse(&timestamp_str, &Rfc3339)
        .map_err(|error| AtomError::Binary(format!("timestamp parse: {error}")))?;
    let provenance = Provenance {
        principal,
        persona,
        agent_run_id,
        tool_call_id,
        trace_id,
        transcript_ref,
        timestamp,
    };

    let inverse_of = match cursor.take_u8()? {
        0 => None,
        1 => {
            let id_bytes = cursor.take(ATOM_ID_BYTES)?;
            let mut id = [0u8; ATOM_ID_BYTES];
            id.copy_from_slice(id_bytes);
            Some(AtomId(id))
        }
        other => return Err(AtomError::Binary(format!("invalid inverse_of tag {other}"))),
    };

    let id_bytes = cursor.take(ATOM_ID_BYTES)?;
    let mut id = [0u8; ATOM_ID_BYTES];
    id.copy_from_slice(id_bytes);
    let id = AtomId(id);

    let principal_key_bytes = cursor.take(ED25519_PUBLIC_KEY_BYTES)?;
    let mut principal_key = [0u8; ED25519_PUBLIC_KEY_BYTES];
    principal_key.copy_from_slice(principal_key_bytes);
    let principal_sig_bytes = cursor.take(ED25519_SIGNATURE_BYTES)?;
    let mut principal_sig = [0u8; ED25519_SIGNATURE_BYTES];
    principal_sig.copy_from_slice(principal_sig_bytes);
    let persona_key_bytes = cursor.take(ED25519_PUBLIC_KEY_BYTES)?;
    let mut persona_key = [0u8; ED25519_PUBLIC_KEY_BYTES];
    persona_key.copy_from_slice(persona_key_bytes);
    let persona_sig_bytes = cursor.take(ED25519_SIGNATURE_BYTES)?;
    let mut persona_sig = [0u8; ED25519_SIGNATURE_BYTES];
    persona_sig.copy_from_slice(persona_sig_bytes);

    if cursor.remaining() != 0 {
        return Err(AtomError::Binary(format!(
            "trailing bytes after atom: {} bytes left",
            cursor.remaining()
        )));
    }

    Ok(Atom {
        id,
        ops,
        parents,
        provenance,
        signature: AtomSignature {
            principal_key,
            principal_sig,
            persona_key,
            persona_sig,
        },
        inverse_of,
    })
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn deterministic_signing_key(seed: u8) -> SigningKey {
        let mut bytes = [0u8; 32];
        for slot in bytes.iter_mut() {
            *slot = seed;
        }
        SigningKey::from_bytes(&bytes)
    }

    fn fixed_provenance() -> Provenance {
        Provenance {
            principal: "user:alice".to_string(),
            persona: "ship-captain".to_string(),
            agent_run_id: "run-0001".to_string(),
            tool_call_id: Some("tc-42".to_string()),
            trace_id: "trace-abcd".to_string(),
            transcript_ref: "transcript:0001#turn-3".to_string(),
            timestamp: OffsetDateTime::parse("2026-04-24T12:34:56Z", &Rfc3339).unwrap(),
        }
    }

    fn sample_ops() -> Vec<TextOp> {
        vec![
            TextOp::Insert {
                offset: 0,
                content: "Hello, ".to_string(),
            },
            TextOp::Insert {
                offset: 7,
                content: "world!".to_string(),
            },
        ]
    }

    fn make_atom() -> Atom {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        Atom::sign(
            sample_ops(),
            Vec::new(),
            fixed_provenance(),
            None,
            &principal,
            &persona,
        )
        .unwrap()
    }

    #[test]
    fn signing_produces_atom_with_verifiable_signatures() {
        let atom = make_atom();
        atom.verify().expect("freshly-signed atom must verify");
    }

    #[test]
    fn tampering_with_ops_invalidates_content_hash() {
        let mut atom = make_atom();
        atom.ops.push(TextOp::Insert {
            offset: 13,
            content: "?!".to_string(),
        });
        let error = atom.verify_content_hash().unwrap_err();
        match error {
            AtomError::ContentHashMismatch { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn tampering_with_signature_fails_verification() {
        let mut atom = make_atom();
        atom.signature.principal_sig[0] ^= 0xff;
        let error = atom.verify_signatures().unwrap_err();
        assert!(matches!(error, AtomError::InvalidSignature("principal")));
    }

    #[test]
    fn inverse_atom_undoes_apply() {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        let atom = make_atom();
        let mut document: Vec<u8> = Vec::new();
        atom.apply(&mut document).unwrap();
        assert_eq!(std::str::from_utf8(&document).unwrap(), "Hello, world!");

        let inverse = Atom::invert(
            &atom,
            Provenance {
                tool_call_id: None,
                ..fixed_provenance()
            },
            &principal,
            &persona,
        )
        .unwrap();

        inverse.verify().unwrap();
        assert_eq!(inverse.inverse_of, Some(atom.id));
        assert_eq!(inverse.parents, vec![atom.id]);

        inverse.apply(&mut document).unwrap();
        assert!(document.is_empty(), "inverse must restore pre-image");
    }

    #[test]
    fn inverse_of_inverse_returns_to_original() {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        let atom = make_atom();
        let inverse = Atom::invert(&atom, fixed_provenance(), &principal, &persona).unwrap();
        let inv_inv = Atom::invert(&inverse, fixed_provenance(), &principal, &persona).unwrap();
        assert_eq!(inv_inv.ops, atom.ops);
    }

    #[test]
    fn json_roundtrip_preserves_atom_id() {
        let atom = make_atom();
        let json = atom.to_json().unwrap();
        let decoded = Atom::from_json_slice(json.as_bytes()).unwrap();
        assert_eq!(decoded, atom);
        assert_eq!(decoded.id, atom.id);
        decoded.verify().unwrap();
    }

    #[test]
    fn binary_roundtrip_preserves_atom_id() {
        let atom = make_atom();
        let bytes = atom.to_binary().unwrap();
        let decoded = Atom::from_binary_slice(&bytes).unwrap();
        assert_eq!(decoded, atom);
        assert_eq!(decoded.id, atom.id);
        decoded.verify().unwrap();
    }

    #[test]
    fn cross_encoding_atom_id_is_stable() {
        let atom = make_atom();
        let from_json = Atom::from_json_slice(atom.to_json().unwrap().as_bytes()).unwrap();
        let from_binary = Atom::from_binary_slice(&atom.to_binary().unwrap()).unwrap();
        assert_eq!(from_json.id, atom.id);
        assert_eq!(from_binary.id, atom.id);
        assert_eq!(from_json.id, from_binary.id);
    }

    #[test]
    fn atom_id_is_deterministic_across_signers() {
        // The AtomId is content-only: re-signing with different keys must
        // produce the same id.
        let principal_a = deterministic_signing_key(11);
        let persona_a = deterministic_signing_key(22);
        let principal_b = deterministic_signing_key(33);
        let persona_b = deterministic_signing_key(44);
        let atom_a = Atom::sign(
            sample_ops(),
            Vec::new(),
            fixed_provenance(),
            None,
            &principal_a,
            &persona_a,
        )
        .unwrap();
        let atom_b = Atom::sign(
            sample_ops(),
            Vec::new(),
            fixed_provenance(),
            None,
            &principal_b,
            &persona_b,
        )
        .unwrap();
        assert_eq!(atom_a.id, atom_b.id);
        assert_ne!(atom_a.signature, atom_b.signature);
    }

    #[test]
    fn binary_decode_rejects_truncated_input() {
        let atom = make_atom();
        let bytes = atom.to_binary().unwrap();
        let truncated = &bytes[..bytes.len() - 1];
        let error = Atom::from_binary_slice(truncated).unwrap_err();
        assert!(matches!(error, AtomError::Binary(_)));
    }

    #[test]
    fn binary_decode_rejects_trailing_bytes() {
        let atom = make_atom();
        let mut bytes = atom.to_binary().unwrap();
        bytes.push(0xff);
        let error = Atom::from_binary_slice(&bytes).unwrap_err();
        assert!(matches!(error, AtomError::Binary(_)));
    }

    #[test]
    fn json_decode_rejects_mismatched_id() {
        let atom = make_atom();
        let mut value: serde_json::Value = serde_json::from_str(&atom.to_json().unwrap()).unwrap();
        let other_id = AtomId([0xaau8; ATOM_ID_BYTES]);
        value["id"] = serde_json::Value::String(other_id.to_hex());
        let raw = serde_json::to_vec(&value).unwrap();
        let error = Atom::from_json_slice(&raw).unwrap_err();
        assert!(matches!(error, AtomError::ContentHashMismatch { .. }));
    }

    #[test]
    fn delete_op_round_trips_apply_and_invert() {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        let mut document = b"abcdef".to_vec();
        let atom = Atom::sign(
            vec![TextOp::Delete {
                offset: 1,
                content: "bcd".to_string(),
            }],
            Vec::new(),
            fixed_provenance(),
            None,
            &principal,
            &persona,
        )
        .unwrap();
        atom.apply(&mut document).unwrap();
        assert_eq!(document, b"aef");
        let inverse = Atom::invert(&atom, fixed_provenance(), &principal, &persona).unwrap();
        inverse.apply(&mut document).unwrap();
        assert_eq!(document, b"abcdef");
    }

    #[test]
    fn delete_op_rejects_content_mismatch() {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        let mut document = b"abcdef".to_vec();
        let atom = Atom::sign(
            vec![TextOp::Delete {
                offset: 0,
                content: "wrong".to_string(),
            }],
            Vec::new(),
            fixed_provenance(),
            None,
            &principal,
            &persona,
        )
        .unwrap();
        let error = atom.apply(&mut document).unwrap_err();
        assert!(matches!(error, AtomError::Apply(_)));
    }

    #[test]
    fn provenance_inverse_of_propagation() {
        let principal = deterministic_signing_key(1);
        let persona = deterministic_signing_key(2);
        let target = make_atom();
        let inverse = Atom::invert(&target, fixed_provenance(), &principal, &persona).unwrap();
        // The inverse atom must reference the exact id of the original.
        assert_eq!(inverse.inverse_of, Some(target.id));
        // Round-tripping through both encodings preserves the inverse_of.
        let from_json = Atom::from_json_slice(inverse.to_json().unwrap().as_bytes()).unwrap();
        let from_binary = Atom::from_binary_slice(&inverse.to_binary().unwrap()).unwrap();
        assert_eq!(from_json.inverse_of, Some(target.id));
        assert_eq!(from_binary.inverse_of, Some(target.id));
    }
}
