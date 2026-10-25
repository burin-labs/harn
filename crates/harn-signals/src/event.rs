//! Rust projection of the `harn.triage_event.v1` envelope.
//!
//! `std/triage` (`stdlib_triage.harn`) owns the shape. These structs refuse
//! unknown fields, and a parity test in `harn-vm` feeds `triage_normalize`
//! output through them, so a field added on the Harn side fails that test
//! instead of being silently dropped on the next re-encode.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::SignalsError;

pub const TRIAGE_EVENT_SCHEMA: &str = "harn.triage_event.v1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageEvent {
    pub schema: String,
    pub id: String,
    pub provider: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_url: String,
    #[serde(default)]
    pub source_timestamp: Option<String>,
    #[serde(default)]
    pub received_at: Option<String>,
    pub actors: Vec<TriageActor>,
    pub summary: String,
    pub why_it_matters: String,
    pub proposed_action: String,
    pub urgency: String,
    pub priority: String,
    pub confidence: f64,
    pub related_refs: Vec<TriageRelatedRef>,
    pub dedupe_key: String,
    pub privacy: TriagePrivacy,
    pub action_intents: Vec<TriageActionIntent>,
    pub raw_payload: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageActor {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageRelatedRef {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriagePrivacy {
    pub redacted: bool,
    pub raw_payload_retained: bool,
    pub contains_sensitive: bool,
    pub flags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageActionIntent {
    pub kind: String,
    pub label: String,
    pub target: JsonValue,
    pub effect: String,
    pub requires_approval: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_duration: Option<String>,
}

/// The fields a producer must supply when it has no `triage_normalize` output.
#[derive(Clone, Debug, Default)]
pub struct ManualEvent {
    pub provider: String,
    pub source_kind: String,
    pub source_url: String,
    pub summary: String,
    pub why_it_matters: String,
    pub proposed_action: String,
    pub urgency: String,
}

impl TriageEvent {
    /// Build an envelope with the same defaults `triage_normalize` applies
    /// when it is handed only these fields. `dedupe_key` is the signal key.
    pub fn manual(input: ManualEvent, dedupe_key: &str) -> Result<Self, SignalsError> {
        let event = Self {
            schema: TRIAGE_EVENT_SCHEMA.to_string(),
            id: format!("triage_evt_{}", short_id(dedupe_key)),
            provider: input.provider,
            source_kind: input.source_kind,
            source_id: input.source_url.clone(),
            source_url: input.source_url.clone(),
            source_timestamp: None,
            received_at: None,
            actors: Vec::new(),
            summary: input.summary,
            why_it_matters: input.why_it_matters,
            proposed_action: input.proposed_action,
            urgency: if input.urgency.is_empty() {
                "normal".to_string()
            } else {
                input.urgency
            },
            priority: "p3".to_string(),
            confidence: 1.0,
            related_refs: vec![TriageRelatedRef {
                kind: "source".to_string(),
                label: None,
                url: input.source_url,
            }],
            dedupe_key: dedupe_key.to_string(),
            privacy: TriagePrivacy {
                redacted: false,
                raw_payload_retained: false,
                contains_sensitive: false,
                flags: Vec::new(),
            },
            action_intents: Vec::new(),
            raw_payload: JsonValue::Object(Default::default()),
        };
        event.validate()?;
        Ok(event)
    }

    /// The checks `triage_validate` applies, at the inbox's write boundary.
    pub fn validate(&self) -> Result<(), SignalsError> {
        if self.schema != TRIAGE_EVENT_SCHEMA {
            return Err(SignalsError::InvalidEvent(format!(
                "unsupported schema `{}`",
                self.schema
            )));
        }
        for (name, value) in [
            ("provider", &self.provider),
            ("source_kind", &self.source_kind),
            ("source_url", &self.source_url),
            ("dedupe_key", &self.dedupe_key),
            ("summary", &self.summary),
            ("proposed_action", &self.proposed_action),
        ] {
            if value.trim().is_empty() {
                return Err(SignalsError::InvalidEvent(format!("{name} is required")));
            }
        }
        for intent in &self.action_intents {
            if intent.effect != "navigation" && !intent.requires_approval {
                return Err(SignalsError::InvalidEvent(format!(
                    "write action intent `{}` must require approval",
                    intent.kind
                )));
            }
        }
        Ok(())
    }

    /// The inbox keeps normalized fields only. Provider payloads can be large
    /// and can carry material the producer did not mean to persist.
    pub(crate) fn strip_raw_payload(&mut self) {
        self.raw_payload = JsonValue::Object(Default::default());
        self.privacy.raw_payload_retained = false;
    }
}

fn short_id(seed: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
        .simple()
        .to_string()[..24]
        .to_string()
}
