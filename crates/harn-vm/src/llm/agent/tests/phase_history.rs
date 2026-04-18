use super::*;

#[test]
fn detects_phase_change_from_latest_loop_state_footer() {
    let text = "First\n\n## LOOP_STATE\nphase: assess\nnext_phase: ground\n## END_LOOP_STATE\n\nSecond\n\n## LOOP_STATE\nphase: ground\nnext_phase: execute\n## END_LOOP_STATE";
    assert!(loop_state_requests_phase_change(text, "ground"));
    assert!(!loop_state_requests_phase_change(text, "execute"));
}

#[test]
fn assistant_history_prefers_canonical_over_raw_text() {
    // Under the tagged response protocol the parser reconstructs a
    // canonical form of the turn. Replaying that form — not the raw
    // provider bytes — is what closes the self-poison loop where leading
    // raw code became "what the agent said" on the next turn.
    let raw_text = "def foo(): pass\n<tool_call>\nread({ path: \"src/lib.rs\" })\n</tool_call>";
    let canonical = "<tool_call>\nread({\n  \"path\": \"src/lib.rs\"\n})\n</tool_call>";
    let tool_calls = vec![json!({"name": "read", "arguments": {"path": "src/lib.rs"}})];

    let replayed = assistant_history_text(Some(canonical), raw_text, 0, &tool_calls);
    assert_eq!(replayed, canonical);
    assert!(
        !replayed.contains("def foo"),
        "raw leading code must NOT leak into replayed history: {replayed}",
    );
}

#[test]
fn assistant_history_falls_back_to_raw_when_no_canonical() {
    // Native tool-call mode and no-tools paths don't run the tagged
    // parser, so canonical is None. In that case we still need a
    // non-empty replay so the model remembers what it said.
    let raw_text = "Short native-mode response.";
    let replayed = assistant_history_text(None, raw_text, 0, &[]);
    assert_eq!(replayed, "Short native-mode response.");
}

#[test]
fn assistant_history_elides_malformed_turns() {
    // When parsing failed we still want a compact placeholder so the next
    // iteration doesn't see (and mutate) its own broken syntax. The
    // placeholder fires irrespective of whether canonical was captured.
    let raw_text = "<tool_call>\nread({ path: 'broken }\n</tool_call>";
    let tool_calls = vec![json!({"name": "read"})];
    let replayed = assistant_history_text(None, raw_text, 2, &tool_calls);
    assert!(replayed.contains("malformed tool call"));
    assert!(!replayed.contains("'broken"));
}
