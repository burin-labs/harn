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
