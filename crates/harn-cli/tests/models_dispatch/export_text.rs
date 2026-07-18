use std::fs;

use super::super::lora_fixtures::{
    write_lora_corpus_fixture, write_lora_generic_placeholder_corpus_fixture,
};
use super::super::support::{parse_json, run, success_data};

#[test]
fn models_lora_export_json_structures_grouped_tool_results() {
    let corpus = write_lora_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["stats"]["records"].as_u64(), Some(6));
    assert_eq!(report["stats"]["emitted"].as_u64(), Some(5));
    assert_eq!(report["stats"]["tool_calls"].as_u64(), Some(5));
    assert_eq!(report["stats"]["tool_results"].as_u64(), Some(5));

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let row = row_text
        .lines()
        .map(|line| parse_json(line, "export row"))
        .find(|row| row["id"] == "parallel-results")
        .expect("parallel row");
    let messages = row["messages"].as_array().expect("messages array");
    let tool_messages = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 2, "messages={messages:?}");
    assert_eq!(tool_messages[0]["name"], "read");
    assert_eq!(tool_messages[0]["tool_call_id"], "call_2_1");
    assert_eq!(tool_messages[0]["content"], "pub fn add() {}");
    assert_eq!(tool_messages[1]["name"], "run");
    assert_eq!(tool_messages[1]["tool_call_id"], "call_2_2");
    assert_eq!(tool_messages[1]["content"], "1 passed");
}

#[test]
fn models_lora_export_text_preserves_declared_no_tool_completion() {
    let corpus = write_lora_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("text.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(
        report["request"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(
        report["stats"]["behavior_strata"]["emitted"]["no_tool_answer"].as_u64(),
        Some(1)
    );
    assert_eq!(
        report["stats"]["behavior_strata"]["emitted"]["unavailable_tool_repair"].as_u64(),
        Some(1)
    );

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let rows = row_text
        .lines()
        .map(|line| parse_json(line, "export row"))
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|row| row["metadata"]["behavior_class"] == "no_tool_answer"
                && row["assistant_tool_text"]
                    .as_str()
                    .is_some_and(|text| !text.contains("<tool_call>"))),
        "rows={rows:?}"
    );
}

#[test]
fn models_lora_export_rejects_generic_placeholder_after_tool_calls() {
    let corpus = write_lora_generic_placeholder_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let errors = harn_value["error"]["details"]["errors"]
        .as_array()
        .expect("errors");
    assert!(
        errors
            .iter()
            .any(|error| error.as_str().is_some_and(|text| text
                .contains("assistant tool calls must be followed by typed tool-result messages"))),
        "errors={errors:?}"
    );
}
