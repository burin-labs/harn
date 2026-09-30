use serde::{Deserialize, Serialize};

use super::AgentTerminalOutcome;

/// Producer-owned phase of an agent turn. Streamed assistant text remains
/// provisional until `Terminal` supplies the final visible reply. A judge's
/// verdict alone does not finalize the turn: another check or wrap-up may run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum AgentTurnPhase {
    Generating,
    /// Covers the whole completion boundary, including deterministic checks,
    /// prechecks, model judges, and arbitration.
    Verifying {
        #[serde(deserialize_with = "deserialize_visible_reply")]
        candidate_reply: String,
    },
    /// The turn stopped. `outcome` distinguishes completion, failure, cancel,
    /// and suspension; `reply` may be empty when a candidate was withdrawn.
    Terminal {
        reply: String,
        outcome: Box<AgentTerminalOutcome>,
    },
}

impl AgentTurnPhase {
    pub(crate) fn from_terminal_record(metadata: &serde_json::Value) -> Option<Self> {
        Some(Self::Terminal {
            reply: metadata.get("visible_reply")?.as_str()?.to_string(),
            outcome: serde_json::from_value(metadata.get("terminal")?.clone()).ok()?,
        })
    }
}

fn deserialize_visible_reply<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    // Use the same projection as assistant chunks and the terminal result.
    // A candidate must not reintroduce control markers or private reasoning.
    Ok(crate::visible_text::sanitize_visible_assistant_text(
        &text, false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_events::{AgentEvent, AgentTerminalKind};

    #[test]
    fn host_boundary_requires_phase_specific_reply_fields() {
        for payload in [
            serde_json::json!({"phase": "verifying"}),
            serde_json::json!({"phase": "terminal", "reply": "draft"}),
            serde_json::json!({"phase": "waiting"}),
        ] {
            assert!(AgentEvent::from_host_payload("s", "turn_phase_changed", &payload).is_err());
        }
        for phase in [
            AgentTurnPhase::Generating,
            AgentTurnPhase::Verifying {
                candidate_reply: "draft".into(),
            },
            AgentTurnPhase::Terminal {
                reply: "final".into(),
                outcome: Box::new(AgentTerminalOutcome::new(
                    AgentTerminalKind::Natural,
                    "done",
                )),
            },
        ] {
            let event = AgentEvent::TurnPhaseChanged {
                session_id: "s".into(),
                phase: phase.clone(),
            };
            let wire = serde_json::to_value(event).unwrap();
            let decoded = AgentEvent::from_host_payload("s", "turn_phase_changed", &wire)
                .unwrap()
                .expect("registered phase");
            match decoded {
                AgentEvent::TurnPhaseChanged { phase: actual, .. } => assert_eq!(actual, phase),
                other => panic!("expected phase, got {other:?}"),
            }
        }
        let candidate = AgentEvent::from_host_payload(
            "s",
            "turn_phase_changed",
            &serde_json::json!({
                "phase": "verifying", "candidate_reply": "draft ##DONE##",
            }),
        )
        .unwrap()
        .expect("candidate phase");
        assert!(matches!(candidate, AgentEvent::TurnPhaseChanged {
            phase: AgentTurnPhase::Verifying { candidate_reply }, ..
        } if candidate_reply == "draft"));
        let invalid = AgentEvent::from_host_payload(
            "s",
            "turn_phase_changed",
            &serde_json::json!({
                "phase": "generating", "reply": "extraneous", "candidate_reply": "extraneous",
            }),
        );
        assert!(
            invalid.is_err(),
            "fields from another phase must be rejected"
        );
    }
}
