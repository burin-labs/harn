//! Typed payload for [`crate::event::SessionEventKind::Control`].
//!
//! The product contract says control words are events. Before this
//! module a stop left no row in the store at all, and a steer, an
//! interrupt and a queued note all read back as the same `Message`
//! row, so an exit authority that wanted to know whether the person
//! running the session had stopped it had to match prose. Prose
//! matching is not a near miss here: the phrase a stop is written in
//! also occurs inside ordinary task prompts, so a heuristic
//! classifies the opening task as a stop.
//!
//! One record therefore carries three things a consumer cannot
//! reconstruct later:
//!
//! 1. `action` — which control word this was.
//! 2. `requested_mode` — the caller's own word, before the canonical
//!    delivery mode collapsed `steer` and `finish_step` onto one
//!    checkpoint.
//! 3. The obligation delta a steer creates: its `message_id`,
//!    `delivery_mode`, and `text`, in the same shape the completion
//!    obligations seam already derives from a delivered message.
//!
//! The record is written at **acceptance**, not at delivery. A steer
//! that is later revoked is followed by its own revoke row rather than
//! being erased, so the stream stays append-only and a reader can see
//! that the control was taken and then withdrawn.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Schema tag carried on every control-event payload.
pub const CONTROL_EVENT_SCHEMA: &str = "harn.session.control.v1";

/// Transcript-event `kind` string that projects onto
/// [`crate::event::SessionEventKind::Control`].
pub const CONTROL_EVENT_KIND: &str = "control";

/// Which control word a surface accepted.
///
/// `Queue` is included even though it never preempts the loop: the
/// point of the record is that a queued note and a steer stop being
/// indistinguishable once they are read back out of the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    /// The run was stopped (ACP `session/cancel`).
    Stop,
    /// A mid-run user directive delivered at the next tool boundary.
    Steer,
    /// A mid-run user directive that preempts the current operation.
    Interrupt,
    /// A note that lands in the transcript after the last model call
    /// and is never rendered into a prompt.
    Queue,
    /// A previously accepted control was withdrawn before delivery.
    Revoke,
}

impl ControlAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Steer => "steer",
            Self::Interrupt => "interrupt",
            Self::Queue => "queue",
            Self::Revoke => "revoke",
        }
    }

    /// Whether the model is expected to have seen this control before
    /// the run reached its next exit decision. `Queue` drains after the
    /// last model call, so it cannot have changed what was asked.
    pub fn is_delivered_to_model(self) -> bool {
        matches!(self, Self::Steer | Self::Interrupt)
    }

    /// Classify an accepted injection by the canonical bridge delivery
    /// mode. `interrupt_immediate` and `finish_step` are the two modes
    /// the completion obligations seam already treats as delivered.
    pub fn from_delivery_mode(mode: &str) -> Self {
        match mode {
            "interrupt_immediate" => Self::Interrupt,
            "finish_step" | "after_current_operation" => Self::Steer,
            _ => Self::Queue,
        }
    }
}

/// One accepted control word, as persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEvent {
    /// Always [`CONTROL_EVENT_SCHEMA`]. Present so a reader can reject
    /// a shape it does not understand instead of silently reading
    /// missing fields as absent facts.
    pub schema: String,
    /// Transcript-event discriminator; always [`CONTROL_EVENT_KIND`],
    /// so the journal's kind-string mapping resolves this payload onto
    /// the typed [`crate::event::SessionEventKind::Control`].
    pub kind: String,
    pub action: ControlAction,
    /// Protocol method that carried the control, e.g. `session/cancel`.
    pub method: String,
    /// Arbitration id shared with the live `control_outcome` event, so
    /// a stored row and a wire notification can be matched up.
    pub control_id: String,
    /// Surface-reported status at acceptance, e.g. `cancelled` or
    /// `already_cancelled`.
    pub status: String,
    /// The caller's own mode word, retained before normalization.
    /// `None` for a stop, which carries no delivery mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_mode: Option<String>,
    /// The canonical bridge delivery mode the caller's word mapped to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_mode: Option<String>,
    /// Identifier of the injected message this control created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The steer text itself. Absent for a stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Who acted, as reported by the surface.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub actor: Value,
    /// How this row was established. A row written at acceptance is
    /// `recorded`. A consumer that recovers a control from prose in a
    /// store written before this schema existed must set `heuristic`,
    /// so a guess can never be mistaken for a record.
    pub provenance: ControlProvenance,
}

/// Whether a control fact was read from a written record or guessed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlProvenance {
    /// Written by the surface at the moment it accepted the control.
    #[default]
    Recorded,
    /// Reconstructed by a consumer from message prose, for stores
    /// written before this schema existed.
    Heuristic,
}

impl ControlEvent {
    /// An accepted stop.
    pub fn stop(
        method: impl Into<String>,
        control_id: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            schema: CONTROL_EVENT_SCHEMA.to_string(),
            kind: CONTROL_EVENT_KIND.to_string(),
            action: ControlAction::Stop,
            method: method.into(),
            control_id: control_id.into(),
            status: status.into(),
            requested_mode: None,
            delivery_mode: None,
            message_id: None,
            text: None,
            actor: Value::Null,
            provenance: ControlProvenance::Recorded,
        }
    }

    /// An accepted injection. `requested_mode` is the caller's word and
    /// `delivery_mode` the canonical bridge mode it normalized to.
    pub fn injection(
        method: impl Into<String>,
        control_id: impl Into<String>,
        status: impl Into<String>,
        requested_mode: impl Into<String>,
        delivery_mode: impl Into<String>,
        message_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let delivery_mode = delivery_mode.into();
        Self {
            schema: CONTROL_EVENT_SCHEMA.to_string(),
            kind: CONTROL_EVENT_KIND.to_string(),
            action: ControlAction::from_delivery_mode(&delivery_mode),
            method: method.into(),
            control_id: control_id.into(),
            status: status.into(),
            requested_mode: Some(requested_mode.into()),
            delivery_mode: Some(delivery_mode),
            message_id: Some(message_id.into()),
            text: Some(text.into()),
            actor: Value::Null,
            provenance: ControlProvenance::Recorded,
        }
    }

    pub fn with_actor(mut self, actor: Value) -> Self {
        self.actor = actor;
        self
    }

    /// Read a control event back out of a stored payload. Returns
    /// `None` when the payload does not carry this schema, so an
    /// unrecognized future shape is a miss rather than a partial read.
    pub fn from_payload(payload: &Value) -> Option<Self> {
        if payload.get("schema").and_then(Value::as_str)? != CONTROL_EVENT_SCHEMA {
            return None;
        }
        serde_json::from_value(payload.clone()).ok()
    }

    /// Read a control event out of a stored session event.
    ///
    /// Gated on the typed kind, so a `Message` row whose text happens to
    /// carry these field names can never be read as a recorded control.
    /// The journal wraps an audit event under `transcript_event`, so both
    /// the bare and the wrapped shape are accepted.
    pub fn from_stored_event(event: &crate::event::StoredEvent) -> Option<Self> {
        if event.kind != crate::event::SessionEventKind::Control {
            return None;
        }
        Self::from_payload(&event.payload).or_else(|| {
            event
                .payload
                .get("transcript_event")
                .and_then(Self::from_payload)
        })
    }

    pub fn to_payload(&self) -> Value {
        serde_json::to_value(self).expect("control event must serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_round_trips_with_the_callers_word_and_the_delivery_mode() {
        let event = ControlEvent::injection(
            "session/inject",
            "ctl-1",
            "accepted",
            "steer",
            "finish_step",
            "msg_inj_abc",
            "if no typed code exists, say so and stop",
        );
        let payload = event.to_payload();
        let read = ControlEvent::from_payload(&payload).expect("typed read");
        assert_eq!(read.action, ControlAction::Steer);
        assert_eq!(read.requested_mode.as_deref(), Some("steer"));
        assert_eq!(read.delivery_mode.as_deref(), Some("finish_step"));
        assert_eq!(read.provenance, ControlProvenance::Recorded);
        assert!(read.action.is_delivered_to_model());
    }

    #[test]
    fn a_queued_note_is_not_a_steer() {
        let event = ControlEvent::injection(
            "session/inject",
            "ctl-2",
            "accepted",
            "queue",
            "audit_only",
            "msg_inj_def",
            "note for later",
        );
        assert_eq!(event.action, ControlAction::Queue);
        assert!(!event.action.is_delivered_to_model());
    }

    #[test]
    fn a_payload_without_the_schema_tag_reads_as_none() {
        assert!(ControlEvent::from_payload(&serde_json::json!({"action": "stop"})).is_none());
    }
}
