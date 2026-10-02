use super::*;
use harn_vm::agent_events::{AgentTerminalKind, AgentTerminalOutcome, AgentTurnPhase};

fn phases() -> [AgentTurnPhase; 3] {
    [
        AgentTurnPhase::Generating,
        AgentTurnPhase::Verifying {
            candidate_reply: "candidate".into(),
        },
        AgentTurnPhase::Terminal {
            reply: "final".into(),
            outcome: Box::new(AgentTerminalOutcome::new(
                AgentTerminalKind::Natural,
                "done",
            )),
        },
    ]
}

pub(super) fn fixture_events() -> Vec<AgentEvent> {
    phases()
        .into_iter()
        .map(|phase| AgentEvent::TurnPhaseChanged {
            session_id: "session-1".into(),
            phase,
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn turn_phases_preserve_candidate_and_terminal_reply_on_acp() {
    let actual = collect_notifications(fixture_events()).await;
    assert_eq!(actual.len(), 3);
    for (notification, phase) in actual.iter().zip(phases()) {
        assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
        let mut expected = serde_json::to_value(phase).unwrap();
        expected["kind"] = serde_json::json!("turn_phase_changed");
        expected["sessionId"] = serde_json::json!("session-1");
        assert_eq!(notification["params"], expected);
    }
}

#[test]
fn wire_schema_refuses_unknown_phase_and_incomplete_replies() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/protocols/schemas/acp-session-update.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    for phase in phases() {
        let mut payload = serde_json::to_value(phase).unwrap();
        payload["kind"] = serde_json::json!("turn_phase_changed");
        payload["sessionId"] = serde_json::json!("session-1");
        let wire = serde_json::json!({"jsonrpc": "2.0", "method": HARN_AGENT_EVENT_METHOD, "params": payload});
        assert!(
            validator.is_valid(&wire),
            "valid phase must satisfy owning schema: {wire}"
        );
        let mut extended = wire.clone();
        extended["params"]["replayed"] = serde_json::json!(true);
        assert!(
            validator.is_valid(&extended),
            "transport metadata must remain valid"
        );
        for other in phases() {
            for (field, value) in serde_json::to_value(other).unwrap().as_object().unwrap() {
                if wire["params"].get(field).is_none() {
                    let mut invalid = wire.clone();
                    invalid["params"][field] = value.clone();
                    assert!(
                        !validator.is_valid(&invalid),
                        "field from another phase cannot pass: {invalid}"
                    );
                }
            }
        }
        let mut invalid = wire.clone();
        invalid["params"]["phase"] = serde_json::json!("waiting");
        assert!(!validator.is_valid(&invalid), "unknown phase cannot pass");
        for required in ["phase", "candidate_reply", "reply", "outcome"] {
            let mut invalid = wire.clone();
            if invalid["params"]
                .as_object_mut()
                .unwrap()
                .remove(required)
                .is_some()
            {
                assert!(
                    !validator.is_valid(&invalid),
                    "missing {required} cannot pass: {invalid}"
                );
            }
        }
    }
}
