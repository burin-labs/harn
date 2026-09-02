//! Session event taxonomy and canonical serialization.
//!
//! Every event a session can persist — `Message`, `ToolCall`,
//! `ToolResult`, `Plan`, `Compaction`, `SystemReminder`, `Hypothesis`,
//! `Receipt`, `Reminder`, `PermissionDecision`, `Control`, plus
//! `Custom { type, payload }` for surface-specific shapes — shares one
//! envelope shape so a TUI session can be read by a cloud verifier and
//! an IDE replay without bespoke per-surface marshalling.
//!
//! The on-the-wire shape is intentionally JSON-first: the storage
//! backend keeps events as canonical UTF-8 bytes, which lets the
//! Ed25519 receipt chain (see [`crate::signing`]) hash the
//! exact bytes that were appended, independent of any structural
//! re-ordering a future Rust struct refactor might introduce.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Identifier for a single event within a session. Monotonic per
/// session; assigned by the store on `append`.
pub type EventId = u64;

/// The named event variants the primitive understands out of the box.
/// `Custom` carries an arbitrary string discriminator so surfaces can
/// extend the taxonomy without forking the schema; the structural
/// validator (see issue #2365) is the authoritative shape gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEventKind {
    Message,
    ToolCall,
    ToolResult,
    Plan,
    Compaction,
    SystemReminder,
    Hypothesis,
    Receipt,
    Reminder,
    PermissionDecision,
    /// An accepted control word — a stop, a steer, an interrupt, or a
    /// queued note — recorded at the moment the surface accepted it.
    ///
    /// The product contract says control words are events. Without this
    /// variant a stop leaves no trace in the store at all, and a steer,
    /// an interrupt and a queued note read back as the same `Message`
    /// row, so an exit authority has to recover "the user stopped this
    /// run" by matching prose. See [`crate::control`] for the payload
    /// shape and the reason the caller's own word is retained beside
    /// the canonical delivery mode.
    Control,
    Custom {
        #[serde(rename = "type")]
        custom_type: String,
    },
}

impl SessionEventKind {
    /// Discriminator string used in canonical hashing and SQL filters.
    pub fn discriminator(&self) -> &str {
        match self {
            Self::Message => "message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Plan => "plan",
            Self::Compaction => "compaction",
            Self::SystemReminder => "system_reminder",
            Self::Hypothesis => "hypothesis",
            Self::Receipt => "receipt",
            Self::Reminder => "reminder",
            Self::PermissionDecision => "permission_decision",
            Self::Control => "control",
            Self::Custom { custom_type } => custom_type.as_str(),
        }
    }

    /// Inverse of [`Self::discriminator`] for the named variants.
    ///
    /// Kept in this `impl` on purpose. A storage backend that hand-wrote
    /// its own string-to-variant table drifted from this one the first
    /// time a variant was added: rows written with the new discriminator
    /// were accepted on the way in and then failed to decode on the way
    /// out. Both directions now come from the same match.
    ///
    /// Returns `None` for `custom`, which needs the caller's stored
    /// custom type to reconstruct.
    pub fn from_discriminator(discriminator: &str) -> Option<Self> {
        Some(match discriminator {
            "message" => Self::Message,
            "tool_call" => Self::ToolCall,
            "tool_result" => Self::ToolResult,
            "plan" => Self::Plan,
            "compaction" => Self::Compaction,
            "system_reminder" => Self::SystemReminder,
            "hypothesis" => Self::Hypothesis,
            "receipt" => Self::Receipt,
            "reminder" => Self::Reminder,
            "permission_decision" => Self::PermissionDecision,
            "control" => Self::Control,
            _ => return None,
        })
    }

    /// Every named variant, for round-trip and coverage checks.
    pub const NAMED: [Self; 11] = [
        Self::Message,
        Self::ToolCall,
        Self::ToolResult,
        Self::Plan,
        Self::Compaction,
        Self::SystemReminder,
        Self::Hypothesis,
        Self::Receipt,
        Self::Reminder,
        Self::PermissionDecision,
        Self::Control,
    ];
}

#[cfg(test)]
mod kind_tests {
    use super::SessionEventKind;

    /// Structural guard. Every named variant must survive a trip through
    /// its discriminator string, so a backend that persists the string
    /// can always decode it again. A new variant that is added to the
    /// enum without a `from_discriminator` arm fails here rather than at
    /// read time on a real session.
    #[test]
    fn every_named_kind_round_trips_through_its_discriminator() {
        for kind in SessionEventKind::NAMED {
            let discriminator = kind.discriminator().to_string();
            assert_eq!(
                SessionEventKind::from_discriminator(&discriminator),
                Some(kind.clone()),
                "{discriminator} does not decode back to its own variant"
            );
        }
    }

    /// The `NAMED` list is itself hand-written, so prove it is complete:
    /// the exhaustive match below stops compiling when a variant is
    /// added, and the assertion fails if it is added without being
    /// listed.
    #[test]
    fn the_named_list_covers_every_non_custom_variant() {
        fn listed(kind: &SessionEventKind) -> bool {
            match kind {
                SessionEventKind::Message
                | SessionEventKind::ToolCall
                | SessionEventKind::ToolResult
                | SessionEventKind::Plan
                | SessionEventKind::Compaction
                | SessionEventKind::SystemReminder
                | SessionEventKind::Hypothesis
                | SessionEventKind::Receipt
                | SessionEventKind::Reminder
                | SessionEventKind::PermissionDecision
                | SessionEventKind::Control => true,
                SessionEventKind::Custom { .. } => false,
            }
        }
        for kind in SessionEventKind::NAMED {
            assert!(listed(&kind), "{kind:?} missing from the exhaustive match");
        }
        assert_eq!(SessionEventKind::NAMED.len(), 11);
    }

    #[test]
    fn custom_and_unknown_discriminators_do_not_decode_to_a_named_variant() {
        assert_eq!(SessionEventKind::from_discriminator("custom"), None);
        assert_eq!(SessionEventKind::from_discriminator("not_a_kind"), None);
    }
}

/// Caller-facing event used on append.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendEvent {
    pub kind: SessionEventKind,
    /// Free-form payload. Validated for null-bytes only — schema
    /// enforcement lives in the structural validator (#2366).
    #[serde(default)]
    pub payload: Value,
    /// Optional caller-asserted parent event id. Stores reject values
    /// that point past the current tail.
    #[serde(default)]
    pub parent_event_id: Option<EventId>,
    /// Optional caller actor (e.g. `"user"`, `"assistant"`, persona id).
    #[serde(default)]
    pub actor: Option<String>,
    /// Tags surface filterable list queries.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Caller-supplied metadata copied verbatim onto the stored event.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl AppendEvent {
    pub fn new(kind: SessionEventKind, payload: Value) -> Self {
        Self {
            kind,
            payload,
            parent_event_id: None,
            actor: None,
            tags: Vec::new(),
            headers: BTreeMap::new(),
        }
    }

    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn with_parent(mut self, parent_event_id: EventId) -> Self {
        self.parent_event_id = Some(parent_event_id);
        self
    }

    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Stamp producer identity into canonical signed headers.
    pub fn with_identity(
        mut self,
        identity: &crate::identity::EventIdentity,
    ) -> Result<Self, crate::identity::EventIdentityError> {
        identity.apply_to_headers(&mut self.headers)?;
        Ok(self)
    }

    /// Read the producer identity already present in canonical headers.
    pub fn identity(
        &self,
    ) -> Result<crate::identity::EventIdentity, crate::identity::EventIdentityError> {
        crate::identity::EventIdentity::from_headers(&self.headers)
    }
}

/// Event as persisted by the store, including assigned identifiers,
/// timestamps, and optional Ed25519 signature material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub event_id: EventId,
    pub session_id: String,
    pub tenant_id: Option<String>,
    pub parent_event_id: Option<EventId>,
    pub actor: Option<String>,
    pub kind: SessionEventKind,
    pub payload: Value,
    pub tags: Vec<String>,
    pub headers: BTreeMap<String, String>,
    pub ts_ms: i64,
    pub ts: String,
    /// Hash of the canonical event bytes (sha256), prefixed with the
    /// algorithm. Forms the chain links: each event's hash is folded
    /// into the next event's `prev_hash`. A confidentiality projection
    /// uses `redacted:<canonical-source-hash>` and cannot be authenticated
    /// as the original row.
    pub record_hash: String,
    /// Hash of the previous event's `record_hash`, or `None` for the
    /// genesis event.
    pub prev_hash: Option<String>,
    /// Detached signature over the canonical event or receipt root. Cleared
    /// when a retrieval hook returns a redacted projection.
    pub signed_by: Option<EventSignature>,
}

impl StoredEvent {
    /// Read producer identity from the signed canonical headers.
    pub fn identity(
        &self,
    ) -> Result<crate::identity::EventIdentity, crate::identity::EventIdentityError> {
        crate::identity::EventIdentity::from_headers(&self.headers)
    }

    /// Whether this event is a confidentiality projection rather than the
    /// signed canonical row. Projections retain the source hash after the
    /// `redacted:` prefix but intentionally clear `signed_by`.
    pub fn is_redacted_projection(&self) -> bool {
        self.record_hash.starts_with("redacted:")
    }

    /// Canonical source hash for a redacted projection, or this event's own
    /// record hash when no projection was required.
    pub fn source_record_hash(&self) -> &str {
        self.record_hash
            .strip_prefix("redacted:")
            .unwrap_or(&self.record_hash)
    }

    pub(crate) fn mark_redacted_projection(&mut self) {
        if !self.is_redacted_projection() {
            self.record_hash = format!("redacted:{}", self.record_hash);
        }
        self.signed_by = None;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSignature {
    /// Algorithm tag, e.g. `"ed25519"`.
    pub algorithm: String,
    /// Public key identifier (sha256 of the verifying key bytes).
    pub key_id: String,
    /// Base64-encoded signature bytes.
    pub signature: String,
}

/// Canonical bytes used for hashing & signing. Keys are sorted so the
/// signature is stable across hashmap iteration order changes.
pub fn canonical_event_bytes(event: &StoredEvent) -> Vec<u8> {
    let canonical = serde_json::json!({
        "schema": "harn.session.event.v1",
        "session_id": event.session_id,
        "event_id": event.event_id,
        "tenant_id": event.tenant_id,
        "parent_event_id": event.parent_event_id,
        "actor": event.actor,
        "kind": event.kind.discriminator(),
        "payload": event.payload,
        "tags": event.tags,
        "headers": event.headers,
        "ts_ms": event.ts_ms,
        "prev_hash": event.prev_hash,
    });
    canonical_json_bytes(&canonical)
}

/// Stable JSON encoding that sorts object keys recursively.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    write_canonical(&mut buf, value);
    buf
}

fn write_canonical(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.extend_from_slice(b"null"),
        Value::Bool(b) => buf.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => buf.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => {
            let encoded = serde_json::to_string(s).expect("string round-trip");
            buf.extend_from_slice(encoded.as_bytes());
        }
        Value::Array(items) => {
            buf.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                write_canonical(buf, item);
            }
            buf.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            buf.push(b'{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                let encoded_key = serde_json::to_string(key).expect("key round-trip");
                buf.extend_from_slice(encoded_key.as_bytes());
                buf.push(b':');
                write_canonical(buf, &map[*key]);
            }
            buf.push(b'}');
        }
    }
}

pub(crate) fn now_ms_and_rfc3339() -> (i64, String) {
    let now = OffsetDateTime::now_utc();
    let ms = (now.unix_timestamp_nanos() / 1_000_000) as i64;
    let text = now.format(&Rfc3339).unwrap_or_else(|_| now.to_string());
    (ms, text)
}

/// Render a millisecond Unix timestamp as RFC 3339. Used by retention
/// sweeps that already have a `now_ms` from the caller and need to
/// stamp tombstone events with the same instant.
pub(crate) fn ms_to_rfc3339(ms: i64) -> String {
    let nanos = (ms as i128).saturating_mul(1_000_000);
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_default()
}
