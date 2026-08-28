//! Compaction and the observation mask.
//!
//! Microcompaction snips large output while preserving short output, strong
//! keyword lines, and multibyte UTF-8 boundaries. Auto-compaction reduces the
//! message count but no-ops under either threshold. The observation mask keeps
//! errors and short results, previews rather than replays a verbose tool call,
//! does not compound a prior recap, honors its budget, collapses repeated lines,
//! and reports recap metrics.

use crate::orchestration::*;
use crate::value::VmDictExt;

#[tokio::test(flavor = "current_thread")]
async fn llm_compaction_call_is_manifested_at_the_provider_boundary() {
    crate::llm::reset_llm_state();
    crate::llm::push_llm_mock(
        crate::llm::parse_llm_mock_value(&serde_json::json!({
            "text": "retained goal and latest evidence"
        }))
        .expect("valid compaction mock"),
    );
    let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
    crate::llm::push_llm_transcript_dir(
        transcript_dir
            .path()
            .to_str()
            .expect("utf8 transcript path"),
    );

    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "mock");
    options.put_str("call_role", "agent.main");
    let llm_opts = crate::llm::extract_llm_options(&[
        crate::value::VmValue::String(arcstr::ArcStr::from("current turn")),
        crate::value::VmValue::String(arcstr::ArcStr::from("parent system prompt")),
        crate::value::VmValue::dict(options),
    ])
    .expect("parent LLM options");
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "old task"}),
        serde_json::json!({
            "role": "assistant",
            "content": "prints/inspects updated after <runtime_feedback kind=\"post_turn\">. The workspace compiles successfully."
        }),
        serde_json::json!({"role": "user", "content": "apply the final edit"}),
        serde_json::json!({
            "role": "tool",
            "name": "edit",
            "content": [{"type": "text", "text": "L81 ERROR: syntax: unexpected '}'"}]
        }),
    ];
    let config = AutoCompactConfig {
        token_threshold: 1,
        keep_last: 2,
        compact_strategy: CompactStrategy::Llm,
        ..Default::default()
    };

    let compacted = auto_compact_messages_with_result(&mut messages, &config, Some(&llm_opts))
        .await
        .expect("LLM compaction succeeds")
        .expect("compaction fired");
    crate::llm::pop_llm_transcript_dir();

    assert_eq!(compacted.strategy, CompactStrategy::Llm);
    assert!(
        compacted
            .summary
            .contains("retained goal and latest evidence"),
        "the mocked compaction result proves the LLM strategy fired"
    );
    assert!(
        !compacted.summary.contains("repair-ledger"),
        "edit-like prose without a mutation receipt must not fabricate a ledger row"
    );
    let mock_calls = crate::llm::get_llm_mock_calls();
    assert_eq!(mock_calls.len(), 1, "compaction reached the mock provider");
    assert_eq!(mock_calls[0].mock_scope, "compaction");
    assert_eq!(mock_calls[0].system, None);
    assert_eq!(mock_calls[0].messages.len(), 1);

    let transcript = std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
        .expect("LLM transcript");
    let events = transcript
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("transcript JSON"))
        .collect::<Vec<_>>();
    let requests = events
        .iter()
        .filter(|event| event["type"] == "provider_call_request")
        .collect::<Vec<_>>();
    assert_eq!(
        requests.len(),
        mock_calls.len(),
        "every recorded provider call must have one request event"
    );
    let request = requests[0];
    assert_eq!(request["call_role"], "compaction");
    assert_eq!(request["served_context"]["system_prompt_bytes"], 0);
    assert_eq!(request["message_count"], 1);
    let manifest_hash = &request["served_context"]["manifest_content_hash"];
    let manifest = events
        .iter()
        .find(|event| {
            event["type"] == "context_manifest" && event["content_hash"] == *manifest_hash
        })
        .expect("request resolves its retained context manifest");
    assert_eq!(manifest["manifest"]["call_role"], "compaction");
    assert_eq!(manifest["manifest"]["root"], "transformed");
    assert_eq!(manifest["manifest"]["system_prompt_bytes"], 0);
}

#[test]
fn microcompact_snips_large_output() {
    let large = "x".repeat(50_000);
    let result = microcompact_tool_output_result(&large, 10_000);
    assert!(result.text.len() < 15_000);
    assert!(result.text.contains("snipped"));
    assert_eq!(result.dropped_bytes, 40_000);
}

#[test]
fn microcompact_preserves_small_output() {
    let small = "hello world";
    let result = microcompact_tool_output(small, 10_000);
    assert_eq!(result, small);
}

#[test]
fn microcompact_preserves_strong_keyword_lines_without_file_line() {
    // Strong keywords ("FAIL", "panic") must preserve the line on their own
    // even without a file:line anchor — they appear on narrative lines (Go
    // "--- FAIL: TestName", Rust "thread '...' panicked at ...",
    // pytest "FAILED tests/..."). Language-specific patterns stay out of the
    // VM; only the generic "strong keyword without file:line" rule lives here.
    let mut output = String::new();
    for i in 0..100 {
        output.push_str(&format!("verbose progress line {i}\n"));
    }
    output.push_str("--- FAIL: TestEmpty (0.00s)\n");
    output.push_str("thread 'tests::test_foo' panicked at src/lib.rs:42:5\n");
    output.push_str("FAILED tests/test_parser.py::test_empty\n");
    for i in 0..100 {
        output.push_str(&format!("more output after failures {i}\n"));
    }
    let result = microcompact_tool_output(&output, 2_000);
    assert!(
        result.contains("--- FAIL: TestEmpty"),
        "strong 'FAIL' keyword should preserve the line:\n{result}"
    );
    assert!(
        result.contains("panicked at"),
        "strong 'panic' keyword should preserve the line:\n{result}"
    );
    assert!(
        result.contains("FAILED tests/test_parser.py"),
        "strong 'FAIL' keyword should preserve pytest-style lines too:\n{result}"
    );
    let accounted = microcompact_tool_output_result(&output, 2_000);
    assert!(
        accounted.dropped_bytes > 0,
        "diagnostic preservation must retain source-loss metadata"
    );
}

#[test]
fn auto_compact_messages_reduces_count() {
    let mut messages: Vec<serde_json::Value> = (0..20)
        .map(|i| serde_json::json!({"role": "user", "content": format!("message {i}")}))
        .collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::Truncate,
            token_threshold: 1,
            keep_last: 6,
            ..Default::default()
        },
        None,
    ));
    let summary = compacted.unwrap();
    assert!(summary.is_some());
    assert!(messages.len() <= 7);
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("auto-compacted"));
}

#[test]
fn auto_compact_noop_when_under_threshold() {
    let mut messages: Vec<serde_json::Value> = (0..4)
        .map(|i| serde_json::json!({"role": "user", "content": format!("msg {i}")}))
        .collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::Truncate,
            keep_last: 6,
            ..Default::default()
        },
        None,
    ));
    assert!(compacted.unwrap().is_none());
    assert_eq!(messages.len(), 4);
}

#[test]
fn auto_compact_noop_when_message_tokens_under_threshold() {
    let mut messages: Vec<serde_json::Value> = (0..20)
        .map(|i| serde_json::json!({"role": "user", "content": format!("short message {i}")}))
        .collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::Truncate,
            token_threshold: 48_000,
            keep_last: 6,
            ..Default::default()
        },
        None,
    ));
    assert!(compacted.unwrap().is_none());
    assert_eq!(messages.len(), 20);
}

#[test]
fn observation_mask_preserves_errors_masks_verbose_output() {
    let verbose_lines: Vec<String> = (0..60)
        .map(|i| format!("// source line {i} of the generated file"))
        .collect();
    let verbose_content = format!(
        "File created: a.go\npackage main\n{}",
        verbose_lines.join("\n")
    );
    let mut messages = vec![
        serde_json::json!({"role": "assistant", "content": "I'll create the file now."}),
        serde_json::json!({"role": "user", "content": verbose_content}),
        serde_json::json!({"role": "assistant", "content": "Now let me run the tests."}),
        serde_json::json!({"role": "user", "content": "error: cannot find module\nexit code 1\nfailed to compile"}),
        serde_json::json!({"role": "assistant", "content": "I see the issue. Let me fix it."}),
        serde_json::json!({"role": "user", "content": "File patched successfully."}),
        serde_json::json!({"role": "assistant", "content": "Running tests again."}),
        serde_json::json!({"role": "user", "content": "All tests passed."}),
    ];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::ObservationMask,
            token_threshold: 1,
            keep_last: 2,
            ..Default::default()
        },
        None,
    ));
    let summary = compacted.unwrap().unwrap();
    assert!(summary.contains("I'll create the file now."));
    assert!(summary.contains("Now let me run the tests."));
    assert!(summary.contains("I see the issue. Let me fix it."));
    assert!(summary.contains("error: cannot find module"));
    assert!(summary.contains("exit code 1"));
    assert!(summary.contains("masked]"));
    assert!(summary.contains("File created: a.go"));
    assert!(!summary.contains("File patched successfully."));
    assert!(!summary.contains("Running tests again."));
    assert!(!summary.contains("All tests passed."));
    assert_eq!(messages.len(), 4);
}

#[test]
fn observation_mask_keeps_short_tool_output() {
    let messages = vec![
        serde_json::json!({"role": "user", "content": "OK"}),
        serde_json::json!({"role": "user", "content": "Done."}),
    ];
    let summary = observation_mask_compaction(&messages, 2);
    assert!(summary.contains("[user] OK"));
    assert!(summary.contains("[user] Done."));
    assert!(!summary.contains("masked"));
}

// --- harn#4731 falsifiers: result-preserving, bounded, non-compounding recap ---

/// Defect #2: the recap must preserve the load-bearing tool RESULT (the failing
/// verify) and must NOT replay the verbatim assistant call that issued it.
#[test]
fn observation_mask_preserves_result_over_verbatim_call_replay() {
    // A long assistant turn carrying a verbose tool CALL (call syntax, not an
    // observation), followed by the tool RESULT with the failing verify output.
    let long_call = format!(
        "Calling the test runner now. {}",
        "cargo test --workspace --all-features -- --nocapture ".repeat(20)
    );
    let failing_result = "running 1 test\n\
        test auth::login_rejects_expired ... FAILED\n\
        assertion `left == right` failed\n\
          left: 401\n\
          right: 200\n\
        error: test failed, to rerun pass `--lib`";
    let messages = vec![
        serde_json::json!({"role": "assistant", "content": long_call}),
        serde_json::json!({"role": "tool", "content": format!("{failing_result}\n{}", "noise line\n".repeat(40))}),
    ];
    let (summary, metrics) =
        observation_mask_compaction_for_test(&messages, messages.len(), DEFAULT_RECAP_BUDGET_BYTES);
    // Primary discriminator (defect #2): the verbatim call syntax must NOT be
    // replayed in full — the assistant turn is reduced to a bounded preview.
    assert!(
        !summary.contains(&"cargo test --workspace --all-features -- --nocapture ".repeat(20)),
        "verbatim call syntax leaked into the recap: {summary}"
    );
    assert!(
        summary.contains("assistant turn truncated"),
        "assistant call replayed verbatim instead of being previewed: {summary}"
    );
    // The load-bearing failing verify result survives, in the masked-marker
    // vocabulary (the tool result is masked but its failure lines are kept).
    assert!(
        summary.contains("test auth::login_rejects_expired ... FAILED"),
        "recap dropped the failing verify result: {summary}"
    );
    assert!(
        summary.contains("chars masked"),
        "masked-marker format not preserved for the tool result: {summary}"
    );
    assert!(metrics.kept_results_count >= 1);
}

/// Defect #3a: a prior recap re-entering the archive window is carried forward
/// ONCE, bounded — two successive compactions do not double (compound) the
/// recap.
#[test]
fn observation_mask_does_not_compound_prior_recap() {
    // First compaction over a large window.
    let first_window: Vec<serde_json::Value> = (0..40)
        .map(|i| {
            serde_json::json!({
                "role": "tool",
                "content": format!("tool result {i}\n{}", "detail line\n".repeat(30)),
            })
        })
        .collect();
    let (recap1, _) = observation_mask_compaction_for_test(
        &first_window,
        first_window.len(),
        DEFAULT_RECAP_BUDGET_BYTES,
    );
    assert!(recap1.contains(RECAP_HEADER_SENTINEL));

    // Second compaction: the prior recap re-enters as a `{role:user}` message
    // alongside fresh activity.
    let mut second_window = vec![serde_json::json!({"role": "user", "content": recap1})];
    for i in 0..40 {
        second_window.push(serde_json::json!({
            "role": "tool",
            "content": format!("new result {i}\n{}", "more detail\n".repeat(30)),
        }));
    }
    let (recap2, metrics2) = observation_mask_compaction_for_test(
        &second_window,
        second_window.len(),
        DEFAULT_RECAP_BUDGET_BYTES,
    );
    assert!(metrics2.carried_prior_recap, "prior recap was not detected");
    // Carried forward exactly once.
    assert_eq!(
        recap2.matches("[prior recap]").count(),
        1,
        "prior recap appears more than once"
    );
    // The recap does not grow without bound across compactions: bounded by the
    // per-message budget plus the single prior-recap carry cap.
    assert!(
        recap2.len() <= DEFAULT_RECAP_BUDGET_BYTES + 8_000,
        "recap compounded to {} bytes",
        recap2.len()
    );
}

/// Defect #3a budget: the recap body honors its byte budget and reports the
/// drop in its receipt.
#[test]
fn observation_mask_honors_budget_and_reports_receipt() {
    // Short results are preserved verbatim (~300 bytes each); 60 of them far
    // exceed the budget, so the tail must be dropped.
    let window: Vec<serde_json::Value> = (0..60)
        .map(|i| {
            serde_json::json!({
                "role": "tool",
                "content": format!("result {i}: {}", "x".repeat(300)),
            })
        })
        .collect();
    let budget = 4_000;
    let (summary, metrics) = observation_mask_compaction_for_test(&window, window.len(), budget);
    assert_eq!(metrics.budget_bytes, budget);
    assert_eq!(metrics.recap_bytes, summary.len());
    assert!(metrics.recap_bytes > 0);
    // Budget is respected (allow one over-budget line plus the trailing drop
    // marker as slack).
    assert!(
        summary.len() <= budget + 4_000,
        "recap {} exceeded budget {budget}",
        summary.len()
    );
    assert!(
        metrics.dropped_count > 0,
        "nothing dropped despite tight budget"
    );
    assert!(summary.contains("dropped to fit recap budget"));
}

/// Repetitive identical rendered lines collapse to a single `(xN)` count.
#[test]
fn observation_mask_collapses_repeated_lines() {
    let window: Vec<serde_json::Value> = (0..5)
        .map(|_| serde_json::json!({"role": "assistant", "content": "read file foo.rs"}))
        .collect();
    let (summary, _) =
        observation_mask_compaction_for_test(&window, window.len(), DEFAULT_RECAP_BUDGET_BYTES);
    assert!(
        summary.contains("(x5)"),
        "repeated lines not collapsed: {summary}"
    );
}

/// The observation-mask strategy surfaces a recap receipt on the
/// `AutoCompactResult` (defect #d), while non-masking strategies do not.
#[test]
fn auto_compact_attaches_recap_metrics_for_observation_mask() {
    let mut messages: Vec<serde_json::Value> = (0..12)
        .map(|i| {
            serde_json::json!({
                "role": "tool",
                "content": format!("result {i}\n{}", "line\n".repeat(60)),
            })
        })
        .collect();
    messages.push(serde_json::json!({"role": "user", "content": "continue"}));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime
        .block_on(auto_compact_messages_with_result(
            &mut messages,
            &AutoCompactConfig {
                compact_strategy: CompactStrategy::ObservationMask,
                token_threshold: 1,
                keep_last: 1,
                ..Default::default()
            },
            None,
        ))
        .unwrap()
        .unwrap();
    let recap = result
        .recap_metrics
        .expect("observation mask must report recap metrics");
    assert_eq!(recap.budget_bytes, DEFAULT_RECAP_BUDGET_BYTES);
    assert!(recap.recap_bytes > 0);
}

#[test]
fn estimate_message_tokens_basic() {
    let messages = vec![
        serde_json::json!({"role": "user", "content": "a".repeat(400)}),
        serde_json::json!({"role": "assistant", "content": "b".repeat(400)}),
    ];
    let tokens = estimate_message_tokens(&messages);
    assert_eq!(tokens, 200);
}

#[test]
fn microcompact_handles_multibyte_utf8() {
    // Slicing at arbitrary byte offsets would panic; these three scripts cover
    // 4/2/3-byte sequences respectively.
    let emoji_output = "🔥".repeat(500);
    let result = microcompact_tool_output(&emoji_output, 400);
    assert!(result.contains("snipped"));

    let mixed = format!("{}{}{}", "a".repeat(300), "é".repeat(500), "b".repeat(300));
    let result2 = microcompact_tool_output(&mixed, 400);
    assert!(result2.contains("snipped"));

    let cjk = "中文".repeat(500);
    let result3 = microcompact_tool_output(&cjk, 400);
    assert!(result3.contains("snipped"));
}
