//! Why a run suspended, carried from the loop's own suspension record to the
//! terminal outcome a host reads.
//!
//! A suspension is the one terminal condition whose cause the loop already
//! holds in typed form and the host could not see. `agent_await_resumption`
//! validates a reason and a `ResumeConditions` at the boundary that owns them,
//! the loop keeps both on its suspend record, and finalize used to hand the
//! host four keys that carried neither. The terminal then said `suspended`
//! twice (as kind and as reason) and nothing else, so a host had to re-derive
//! the cause by scanning the transcript for a denial.
//!
//! This module is the typed shape of that cause. It is parsed once here, from
//! the wire dict the loop passes to finalize, and every consumer reads the
//! struct rather than the dict.

use serde::{Deserialize, Serialize};

/// What the loop is waiting for before it can resume.
///
/// One variant per `ResumeConditions` field, validated upstream by
/// `parse_resume_conditions`. `Trigger` deliberately does not copy the trigger
/// spec: the spec is an open dict owned by the trigger registry, and the
/// terminal record's job is to name what the run waits on, not to republish a
/// registry document on a protocol field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ResumeWait {
    /// Waiting for a runtime event topic, e.g. `workspace_trusted`.
    Event(String),
    /// Waiting until a deadline, with the action taken when it expires.
    Timeout {
        duration_minutes: i64,
        on_timeout: String,
    },
    /// Waiting on a registered trigger spec.
    Trigger,
}

impl ResumeWait {
    /// The stable token a host matches on. Readable, but the shape is the
    /// contract: `<field>:<value>` with no spaces, so a host branches on a
    /// prefix instead of parsing prose.
    fn token(&self) -> String {
        match self {
            Self::Event(topic) => format!("on_event:{topic}"),
            Self::Timeout {
                duration_minutes,
                on_timeout,
            } => format!("timeout:{duration_minutes}m/{on_timeout}"),
            Self::Trigger => "trigger".to_string(),
        }
    }
}

/// The typed cause of a suspended terminal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentTerminalSuspension {
    /// The reason the suspending call declared. Required by
    /// `agent_await_resumption`, so it is never empty on a real suspension.
    pub reason: String,
    /// Who parked the run: `self` for a model-initiated await.
    pub initiator: String,
    /// Every condition the loop is waiting on, in `ResumeConditions` field
    /// order. Empty means the run parked with no declared resume condition.
    pub waits: Vec<ResumeWait>,
}

impl AgentTerminalSuspension {
    /// Read the suspension record the loop passes to finalize.
    ///
    /// Returns `None` for anything that is not a suspension record with a
    /// non-empty reason, so a caller cannot accidentally attach an empty cause
    /// that reads as "we looked and there was nothing".
    pub fn from_status_value(value: Option<&serde_json::Value>) -> Option<Self> {
        let value = value?;
        let reason = value.get("reason").and_then(serde_json::Value::as_str)?;
        let reason = reason.trim();
        if reason.is_empty() {
            return None;
        }
        let initiator = value
            .get("initiator")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some(Self {
            reason: reason.to_string(),
            initiator,
            waits: waits_from_conditions(value.get("conditions")),
        })
    }

    /// The `detail` projection: the resume conditions as stable tokens, or
    /// `None` when the run declared none. Absence here means "no condition was
    /// declared", never "we did not look".
    pub fn detail(&self) -> Option<String> {
        if self.waits.is_empty() {
            return None;
        }
        let tokens: Vec<String> = self.waits.iter().map(ResumeWait::token).collect();
        Some(format!("resume_when={}", tokens.join(",")))
    }
}

fn waits_from_conditions(conditions: Option<&serde_json::Value>) -> Vec<ResumeWait> {
    let Some(conditions) = conditions.filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let mut waits = Vec::new();
    if conditions
        .get("trigger")
        .is_some_and(serde_json::Value::is_object)
    {
        waits.push(ResumeWait::Trigger);
    }
    if let Some(timeout) = conditions.get("timeout").filter(|value| value.is_object()) {
        if let Some(duration_minutes) = timeout
            .get("duration_minutes")
            .and_then(serde_json::Value::as_i64)
        {
            waits.push(ResumeWait::Timeout {
                duration_minutes,
                on_timeout: timeout
                    .get("on_timeout")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    if let Some(topic) = conditions
        .get("on_event")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
    {
        waits.push(ResumeWait::Event(topic.to_string()));
    }
    waits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trust_denial_suspension_carries_its_reason_and_the_event_it_waits_on() {
        let suspension = AgentTerminalSuspension::from_status_value(Some(&serde_json::json!({
            "reason": "Workspace is untrusted, so the edit was refused",
            "initiator": "self",
            "conditions": {"on_event": "workspace_trusted"},
        })))
        .expect("a suspension record with a reason parses");

        assert_eq!(
            suspension.reason,
            "Workspace is untrusted, so the edit was refused"
        );
        assert_eq!(
            suspension.waits,
            vec![ResumeWait::Event("workspace_trusted".to_string())]
        );
        assert_eq!(
            suspension.detail().as_deref(),
            Some("resume_when=on_event:workspace_trusted")
        );
    }

    #[test]
    fn a_record_without_a_reason_is_not_a_cause() {
        assert!(AgentTerminalSuspension::from_status_value(None).is_none());
        assert!(
            AgentTerminalSuspension::from_status_value(Some(&serde_json::json!({
                "initiator": "self"
            })))
            .is_none()
        );
        assert!(
            AgentTerminalSuspension::from_status_value(Some(&serde_json::json!({
                "reason": "   "
            })))
            .is_none()
        );
    }

    #[test]
    fn no_declared_condition_leaves_detail_absent_rather_than_empty() {
        let suspension = AgentTerminalSuspension::from_status_value(Some(&serde_json::json!({
            "reason": "waiting on the operator",
            "initiator": "self",
            "conditions": serde_json::Value::Null,
        })))
        .expect("a reason alone is a cause");

        assert!(suspension.waits.is_empty());
        assert_eq!(suspension.detail(), None);
    }

    #[test]
    fn every_declared_condition_reaches_the_detail_token() {
        let suspension = AgentTerminalSuspension::from_status_value(Some(&serde_json::json!({
            "reason": "parked",
            "initiator": "self",
            "conditions": {
                "trigger": {"kind": "cron"},
                "timeout": {"duration_minutes": 5, "on_timeout": "resume"},
                "on_event": "workspace_trusted",
            },
        })))
        .expect("a full condition set parses");

        assert_eq!(
            suspension.detail().as_deref(),
            Some("resume_when=trigger,timeout:5m/resume,on_event:workspace_trusted")
        );
    }
}
