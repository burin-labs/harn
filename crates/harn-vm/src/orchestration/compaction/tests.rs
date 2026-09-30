use super::*;

#[test]
fn microcompact_short_output_unchanged() {
    let output = "line1\nline2\nline3\n";
    assert_eq!(microcompact_tool_output(output, 1000), output);
}

#[test]
fn microcompact_snaps_to_line_boundaries() {
    let lines: Vec<String> = (0..20)
        .map(|i| format!("line {i:02} content here"))
        .collect();
    let output = lines.join("\n");
    let result = microcompact_tool_output(&output, 200);
    assert!(result.contains("[... "), "should have snip marker");
    let parts: Vec<&str> = result.split("\n\n[... ").collect();
    assert!(parts.len() >= 2, "should split at marker");
    let head = parts[0];
    for line in head.lines() {
        assert!(
            line.starts_with("line "),
            "head line should be complete: {line}"
        );
    }
}

#[test]
fn microcompact_preserves_diagnostic_lines_with_line_boundaries() {
    let mut lines = Vec::new();
    for i in 0..50 {
        lines.push(format!("verbose output line {i}"));
    }
    lines.push("src/main.rs:42: error: cannot find value".to_string());
    for i in 50..100 {
        lines.push(format!("verbose output line {i}"));
    }
    let output = lines.join("\n");
    let result = microcompact_tool_output(&output, 600);
    assert!(result.contains("cannot find value"), "diagnostic preserved");
    assert!(
        result.contains("[diagnostic lines preserved]"),
        "has diagnostic marker"
    );
}

// D1-durable: the shared failure-signal filter must keep the structured
// failure kinds the old mask path dropped — assertion values, rustc
// help/caret/source rows, and `Lnnn:` markers — not just keyword lines.
#[test]
fn failure_signal_filter_keeps_structured_failure_lines() {
    for keep in [
        "left: 3",
        "right: 4",
        "expected: foo",
        "actual: bar",
        "  --> src/main.rs:4:9",
        "= help: add `use std::fmt;`",
        "12 | let x = bad();",
        "   | ^^^^^^^ not found",
        "L42: assertion failed",
        "src/main.rs:42: error: cannot find value",
        "FAIL: TestThing",
        "panic: index out of range",
    ] {
        assert!(
            is_failure_signal_line(keep),
            "should keep failure-signal line: {keep:?}"
        );
    }
    for drop in [
        "verbose output line 7",
        "compiling crate foo",
        "    let y = ok();",
        "",
    ] {
        assert!(
            !is_failure_signal_line(drop),
            "should drop ordinary line: {drop:?}"
        );
    }
}

// D1-durable: masking a large tool output must preserve the assertion
// values and rustc detail (not just the first line) so the model can fix
// the bug instead of re-reading a shredded summary.
#[test]
fn default_mask_preserves_failure_detail() {
    let mut lines = vec!["running 1 test".to_string()];
    for i in 0..40 {
        lines.push(format!("noise line {i}"));
    }
    lines.push("assertion `left == right` failed".to_string());
    lines.push("  left: 3".to_string());
    lines.push(" right: 4".to_string());
    lines.push("  --> src/lib.rs:10:5".to_string());
    for i in 40..80 {
        lines.push(format!("more noise {i}"));
    }
    let content = lines.join("\n");
    let masked = default_mask_tool_result("tool", &content);
    assert!(
        masked.contains("masked"),
        "still reports it masked: {masked}"
    );
    assert!(
        masked.contains("failure lines preserved"),
        "should flag preserved lines: {masked}"
    );
    assert!(masked.contains("left: 3"), "keeps left value: {masked}");
    assert!(masked.contains("right: 4"), "keeps right value: {masked}");
    assert!(
        masked.contains("--> src/lib.rs:10:5"),
        "keeps rustc location: {masked}"
    );
    assert!(
        !masked.contains("noise line 7"),
        "drops ordinary noise: {masked}"
    );
}

// No failure signal → terse mask; a multibyte tail at byte 120 panicked.
#[test]
fn default_mask_without_failure_lines_stays_terse() {
    let mut lines: Vec<String> = (0..40).map(|i| format!("plain line {i}")).collect();
    lines[0] = format!("{}日本語テキスト", "x".repeat(118));
    let masked = default_mask_tool_result("tool", &lines.join("\n"));
    assert!(masked.contains("masked]"), "should mask: {masked}");
    assert!(
        !masked.contains("failure lines preserved"),
        "no failure lines to preserve: {masked}"
    );
}

#[test]
fn token_estimate_counts_structured_message_content() {
    let text = "x".repeat(400);
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": [
            {"type": "text", "text": text},
            {"type": "input_text", "text": "tail"},
        ],
        "reasoning": {"text": "scratch"},
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "read", "arguments": "{\"path\":\"src/main.rs\"}"}
        }],
    })];

    assert!(
        estimate_message_tokens(&messages) >= 100,
        "structured content must not count as zero"
    );
}

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
fn snap_to_line_end_finds_newline() {
    let s = "line1\nline2\nline3\nline4\n";
    let head = snap_to_line_end(s, 12);
    assert!(head.ends_with('\n'), "should end at newline");
    assert!(head.contains("line1"));
}

#[test]
fn snap_to_line_start_finds_newline() {
    let s = "line1\nline2\nline3\nline4\n";
    let tail = snap_to_line_start(s, 12);
    assert!(
        tail.starts_with("line"),
        "should start at line boundary: {tail}"
    );
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

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
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

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
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
fn snap_split_off_tool_results_handles_all_result_shapes() {
    // A split pointing at any tool-result shape walks back to the
    // request that initiated the run. OpenAI durable shape
    // (`role: "tool"`):
    let openai = vec![
        serde_json::json!({"role": "user", "content": "task"}),
        serde_json::json!({"role": "assistant", "content": "", "tool_calls": []}),
        serde_json::json!({"role": "tool", "tool_call_id": "c0", "content": "r0"}),
    ];
    assert_eq!(snap_split_off_tool_results(&openai, 2, 0), 1);
    // Anthropic durable shape (`role: "tool_result"`).
    let anthropic = vec![
        serde_json::json!({"role": "user", "content": "task"}),
        serde_json::json!({"role": "assistant", "content": ""}),
        serde_json::json!({"role": "tool_result", "tool_use_id": "c0", "content": "r0"}),
    ];
    assert_eq!(snap_split_off_tool_results(&anthropic, 2, 0), 1);
    // User message carrying tool_result blocks.
    let user_blocks = vec![
        serde_json::json!({"role": "user", "content": "task"}),
        serde_json::json!({"role": "assistant", "content": ""}),
        serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "c0", "content": "r0"}],
        }),
    ];
    assert_eq!(snap_split_off_tool_results(&user_blocks, 2, 0), 1);
    // Plain user text is a safe boundary — untouched.
    let text = vec![
        serde_json::json!({"role": "assistant", "content": ""}),
        serde_json::json!({"role": "user", "content": "plain"}),
    ];
    assert_eq!(snap_split_off_tool_results(&text, 1, 0), 1);
    // Backward walk pinned at compact_start: fall forward past the run
    // so compaction still makes progress (the whole pair is drained
    // together rather than split).
    let pinned = vec![
        serde_json::json!({"role": "tool", "tool_call_id": "c0", "content": "r0"}),
        serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "r1"}),
        serde_json::json!({"role": "assistant", "content": "done"}),
    ];
    assert_eq!(snap_split_off_tool_results(&pinned, 1, 0), 2);
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

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
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

/// (1) A pinned tool-output survives an observation-mask pass that evicts
/// (masks) the unpinned verbose outputs around it.
#[test]
fn observation_mask_preserves_pinned_live_file_view() {
    let pinned_body = format!(
        "## Edited region now reads (line 42, ±6 context) {}\n```\n{}\n```",
        NO_COMPACT_MARKER,
        (0..40)
            .map(|i| format!("   {i}  let x = compute({i});"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let verbose_unpinned = (0..60)
        .map(|i| format!("verbose scan output line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    // These are the ARCHIVED messages handed to the mask pass.
    let archived = vec![
        serde_json::json!({"role": "user", "content": verbose_unpinned}),
        serde_json::json!({"role": "user", "content": pinned_body}),
    ];
    let summary = observation_mask_compaction(&archived, archived.len());
    // Pinned live file view survives verbatim.
    assert!(
        summary.contains("Edited region now reads"),
        "pinned heading survived: {summary}"
    );
    assert!(
        summary.contains("let x = compute(39);"),
        "pinned body survived verbatim"
    );
    // The unpinned verbose neighbor was masked.
    assert!(summary.contains("masked]"), "unpinned output was masked");
    assert!(!summary.contains("verbose scan output line 30"));
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
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
