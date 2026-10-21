use super::*;

#[test]
fn compaction_policy_instructions_extend_by_default() {
    let policy = CompactionPolicy {
        instructions: Some("Keep the failing test names.".to_string()),
        ..Default::default()
    };
    let archived = [serde_json::json!({"role": "user", "content": "old context"})];
    let retained = [serde_json::json!({"role": "tool", "content": "new evidence"})];
    let prompt = render_llm_compaction_prompt(None, &archived, &retained, 1, &policy)
        .expect("prompt renders");

    assert_eq!(policy.instruction_mode(), "extend");
    assert!(prompt.contains("Preserve goals, constraints"));
    assert!(prompt.contains("Additional compaction instructions"));
    assert!(prompt.contains("Keep the failing test names."));
    assert!(prompt.contains("TOOL: new evidence"));
}

#[test]
fn compaction_policy_can_replace_default_instructions() {
    let policy = CompactionPolicy {
        instructions: Some("Only keep repro steps.".to_string()),
        extend_default_instructions: Some(false),
        ..Default::default()
    };
    let archived = [serde_json::json!({"role": "user", "content": "old context"})];
    let retained = [serde_json::json!({"role": "tool", "content": "new evidence"})];
    let prompt = render_llm_compaction_prompt(None, &archived, &retained, 1, &policy)
        .expect("prompt renders");

    assert_eq!(policy.instruction_mode(), "replace");
    assert!(prompt.contains("according to these instructions"));
    assert!(prompt.contains("Only keep repro steps."));
    assert!(!prompt.contains("Preserve goals, constraints"));
    assert!(prompt.contains("newer than every archived message"));
    assert!(prompt.contains("TOOL: new evidence"));
}

#[test]
fn auto_compact_preserves_reasoning_tool_suffix() {
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "old task"}),
        serde_json::json!({"role": "assistant", "content": "old reply"}),
        serde_json::json!({"role": "user", "content": "new task"}),
        serde_json::json!({
            "role": "assistant",
            "content": "",
            "reasoning": "think first",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"foo.rs\"}"}
            }],
        }),
        serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": "file"}),
    ];
    let config = AutoCompactConfig {
        token_threshold: 1,
        keep_last: 2,
        ..Default::default()
    };

    let runtime = crate::test_support::current_thread_runtime();
    let summary = runtime
        .block_on(auto_compact_messages(&mut messages, &config, None))
        .expect("compaction succeeds");

    assert!(summary.is_some());
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");
}

/// Regression (transcript integrity): a tool-heavy transcript whose only
/// user message is the pinned head has no interior user boundary, so the
/// split falls back to the naive `len - keep_last` index — which can land
/// BETWEEN an assistant tool_use message and its tool_result, orphaning
/// the result at the kept-window head. The split must snap to the start
/// of the request/result pair instead.
#[test]
fn auto_compact_never_splits_assistant_tool_use_from_its_result() {
    let tool_call = |id: &str| {
        serde_json::json!({
            "id": id,
            "type": "function",
            "function": {"name": "run", "arguments": "{}"}
        })
    };
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "task"}),
        serde_json::json!({"role": "assistant", "content": "", "tool_calls": [tool_call("c0")]}),
        serde_json::json!({"role": "tool", "tool_call_id": "c0", "content": "r0"}),
        serde_json::json!({"role": "assistant", "content": "", "tool_calls": [tool_call("c1")]}),
        serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "r1"}),
        serde_json::json!({"role": "assistant", "content": "", "tool_calls": [tool_call("c2")]}),
        serde_json::json!({"role": "tool", "tool_call_id": "c2", "content": "r2"}),
    ];
    // keep_last: 3 puts the naive split at index 4 — the tool_result for
    // c1 — exactly mid-pair.
    let config = AutoCompactConfig {
        token_threshold: 1,
        keep_first: 0,
        keep_last: 3,
        ..Default::default()
    };

    let runtime = crate::test_support::current_thread_runtime();
    let summary = runtime
        .block_on(auto_compact_messages(&mut messages, &config, None))
        .expect("compaction succeeds");
    assert!(summary.is_some(), "compaction should trigger");

    // Kept window: summary, then the INTACT c1 pair, then the c2 pair.
    assert_eq!(messages[0]["role"], "user", "summary head");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "c1");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "c1");
    assert_eq!(messages[3]["tool_calls"][0]["id"], "c2");
    assert_eq!(messages[4]["tool_call_id"], "c2");
    // No kept tool_result may reference a drained (missing) request.
    for (idx, message) in messages.iter().enumerate() {
        if message["role"] == "tool" {
            let id = message["tool_call_id"].as_str().expect("tool_call_id");
            let paired = messages[..idx].iter().any(|prev| {
                prev["tool_calls"]
                    .as_array()
                    .is_some_and(|calls| calls.iter().any(|call| call["id"] == id))
            });
            assert!(paired, "tool_result {id} orphaned in kept window");
        }
    }
}

#[test]
fn auto_compact_clamps_oversized_tool_output_to_max_chars() {
    // A large tool result in the *kept* window must be clamped to honor
    // `tool_output_max_chars`.
    let big = "x".repeat(4000);
    let big_len = big.len();
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "old task"}),
        serde_json::json!({"role": "assistant", "content": "old reply"}),
        serde_json::json!({"role": "user", "content": "new task"}),
        serde_json::json!({"role": "assistant", "content": "calling tool"}),
        serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": big}),
    ];
    let config = AutoCompactConfig {
        token_threshold: 1,
        keep_last: 2,
        tool_output_max_chars: 500,
        ..Default::default()
    };

    let runtime = crate::test_support::current_thread_runtime();
    let result = runtime
        .block_on(auto_compact_messages(&mut messages, &config, None))
        .expect("compaction succeeds");
    assert!(result.is_some(), "compaction should trigger");

    let tool_msg = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("tool message kept in window");
    // Pairing preserved...
    assert_eq!(tool_msg["tool_call_id"], "call_1");
    // ...and the oversized body was clamped well below its original size.
    let content = tool_msg["content"].as_str().expect("string content");
    assert!(
        content.len() < big_len,
        "tool output should be clamped: {} vs {}",
        content.len(),
        big_len
    );
    assert!(content.len() < 2000, "clamped near tool_output_max_chars");
}

/// (2) A pinned large tool-output is NOT clamped, while an unpinned one of
/// the same size IS.
#[test]
fn clamp_exempts_pinned_tool_output() {
    let pinned_big = format!(
        "## Exact current file text {}\n{}",
        NO_COMPACT_MARKER,
        "x".repeat(4000)
    );
    let pinned_len = pinned_big.len();
    let unpinned_big = "y".repeat(4000);
    let unpinned_len = unpinned_big.len();
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "old task"}),
        serde_json::json!({"role": "assistant", "content": "reply"}),
        serde_json::json!({"role": "user", "content": "new task"}),
        serde_json::json!({"role": "assistant", "content": "calling tools"}),
        serde_json::json!({"role": "tool", "tool_call_id": "c0", "content": unpinned_big}),
        serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": pinned_big}),
        serde_json::json!({"role": "user", "content": "continue"}),
    ];
    let config = AutoCompactConfig {
        token_threshold: 1,
        keep_last: 4,
        tool_output_max_chars: 500,
        ..Default::default()
    };
    let runtime = crate::test_support::current_thread_runtime();
    runtime
        .block_on(auto_compact_messages(&mut messages, &config, None))
        .expect("compaction succeeds");

    let pinned_msg = messages
        .iter()
        .find(|m| m["tool_call_id"] == "c1")
        .expect("pinned tool message kept");
    assert_eq!(
        pinned_msg["content"].as_str().map(str::len),
        Some(pinned_len),
        "pinned output must be intact (unclamped)"
    );
    let unpinned_msg = messages
        .iter()
        .find(|m| m["tool_call_id"] == "c0")
        .expect("unpinned tool message kept");
    assert!(
        unpinned_msg["content"].as_str().map(str::len).unwrap() < unpinned_len,
        "unpinned output of the same size must be clamped"
    );
}

/// (3) Bounded policy: with MANY pinned outputs, only the latest
/// MAX_PINNED_SEGMENTS survive verbatim; older pinned duplicates compact —
/// so the pin can't prevent all compaction (and can't overflow the window
/// on a very long session).
#[test]
fn pin_bound_keeps_only_latest_segments() {
    // Build 6 distinct pinned, oversized edited-window snapshots
    // (gen 0 = oldest .. gen 5 = newest), each tagged with the marker and
    // long enough that masking would otherwise truncate it.
    let make = |gen: usize| {
        let body = (0..40)
            .map(|i| format!("marker-gen-{gen} body line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::json!({
            "role": "user",
            "content": format!(
                "## Edited region now reads (gen {gen}) {}\n{}",
                NO_COMPACT_MARKER, body
            ),
        })
    };
    let archived: Vec<_> = (0..6).map(make).collect();

    // Unit-level: the index selection keeps exactly the latest N.
    let pinned = latest_pinned_indices(archived.iter(), |m| {
        m.get("content").and_then(|c| c.as_str())
    });
    assert_eq!(
        pinned.len(),
        MAX_PINNED_SEGMENTS,
        "only the latest MAX_PINNED_SEGMENTS are pinned"
    );
    assert!(pinned.contains(&5) && pinned.contains(&4) && pinned.contains(&3));
    assert!(!pinned.contains(&0) && !pinned.contains(&1) && !pinned.contains(&2));

    // End-to-end through the mask pass: the 3 newest snapshots survive
    // verbatim; the 3 oldest are masked, proving the pin cannot defeat all
    // compaction.
    let summary = observation_mask_compaction(&archived, archived.len());
    assert!(
        summary.contains("marker-gen-5")
            && summary.contains("marker-gen-4")
            && summary.contains("marker-gen-3"),
        "latest {MAX_PINNED_SEGMENTS} pinned snapshots survive verbatim: {summary}"
    );
    assert!(
        !summary.contains("marker-gen-0")
            && !summary.contains("marker-gen-1")
            && !summary.contains("marker-gen-2"),
        "older pinned snapshots are masked (bound enforced)"
    );
    assert!(summary.contains("masked]"), "older snapshots were masked");
}

/// (4) Regression: with NO pins, compaction behaves exactly as before.
#[test]
fn no_pins_preserves_prior_clamp_behavior() {
    let big = "x".repeat(4000);
    let big_len = big.len();
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "old task"}),
        serde_json::json!({"role": "assistant", "content": "old reply"}),
        serde_json::json!({"role": "user", "content": "new task"}),
        serde_json::json!({"role": "assistant", "content": "calling tool"}),
        serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": big}),
    ];
    let config = AutoCompactConfig {
        token_threshold: 1,
        keep_last: 2,
        tool_output_max_chars: 500,
        ..Default::default()
    };
    let runtime = crate::test_support::current_thread_runtime();
    let result = runtime
        .block_on(auto_compact_messages(&mut messages, &config, None))
        .expect("compaction succeeds");
    assert!(result.is_some());
    let tool_msg = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool kept");
    let content = tool_msg["content"].as_str().expect("string content");
    assert!(content.len() < big_len, "unpinned output clamped as before");
    assert!(content.len() < 2000, "clamped near tool_output_max_chars");
}
