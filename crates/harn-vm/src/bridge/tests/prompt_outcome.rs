use super::test_bridge;
use crate::agent_events::{AgentTerminalKind, AgentTerminalOutcome};
use crate::bridge::PromptTerminalReceipt;
use serde_json::{json, Value};

#[test]
fn prompt_terminal_receipt_is_atomic_last_writer_wins_and_consumed_once() {
    let bridge = test_bridge();
    assert!(bridge.take_prompt_terminal_receipt().is_none());
    for outcome in [
        Some(json!({"kind":"not_achieved", "evidence_id":"inner"})),
        Some(json!({"kind":"unknown", "evidence_id":"outer"})),
        Some(Value::Null),
        None,
    ] {
        bridge.set_prompt_terminal_receipt(PromptTerminalReceipt {
            stop_reason: "end_turn".into(),
            terminal: AgentTerminalOutcome::new(AgentTerminalKind::Natural, "finished"),
            goal_outcome: Some(json!({"kind":"achieved", "evidence_id":"old"})),
        });
        bridge.set_prompt_terminal_receipt(PromptTerminalReceipt {
            stop_reason: "cancelled".into(),
            terminal: AgentTerminalOutcome::new(AgentTerminalKind::UserCancelled, "stopped"),
            goal_outcome: outcome.clone(),
        });
        let receipt = bridge
            .take_prompt_terminal_receipt()
            .expect("current receipt");
        assert_eq!(receipt.stop_reason, "cancelled");
        assert_eq!(receipt.terminal.kind, AgentTerminalKind::UserCancelled);
        assert_eq!(receipt.goal_outcome, outcome);
        assert!(bridge.take_prompt_terminal_receipt().is_none());
    }
}

#[test]
#[allow(deprecated)]
fn legacy_prompt_api_delegates_without_inventing_goal_measurement() {
    let bridge = test_bridge();
    let terminal = AgentTerminalOutcome::new(AgentTerminalKind::Natural, "finished");
    bridge.set_prompt_terminal_receipt(PromptTerminalReceipt {
        stop_reason: "end_turn".into(),
        terminal: terminal.clone(),
        goal_outcome: Some(json!({"kind":"achieved"})),
    });
    bridge.set_prompt_outcome("end_turn", &terminal);
    let receipt = bridge
        .take_prompt_terminal_receipt()
        .expect("legacy receipt");
    assert_eq!(receipt.terminal, terminal);
    assert_eq!(receipt.goal_outcome, None);
    bridge.set_prompt_terminal_receipt(receipt);
    let (reason, legacy_terminal) = bridge.take_prompt_outcome().expect("legacy reader");
    assert_eq!(reason, "end_turn");
    assert_eq!(legacy_terminal, terminal);
    assert!(bridge.take_prompt_terminal_receipt().is_none());
}
