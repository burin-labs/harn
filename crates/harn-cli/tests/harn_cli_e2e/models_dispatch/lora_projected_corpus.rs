//! `harn models lora export` over Harn-projected training examples.
//!
//! A projected row already carries typed calls, typed results, and the exact
//! catalog the model was served. These checks pin the two properties that
//! makes it worth projecting: the catalog flows through untouched, and a row
//! whose call/result pairing is broken is refused rather than exported into a
//! corpus that later reads as valid.

use std::fs;

use serde_json::{json, Value};

use super::support::{parse_json, run};

/// The export report, whichever envelope arm it landed in.
///
/// A single-row corpus cannot satisfy the exporter's behavior-strata
/// composition gate, so these checks read the report body directly instead of
/// treating the exit code as the verdict. Strata coverage is the curator's
/// concern; what is under test here is what the projected row converted into.
fn report_body(stdout: &str, label: &str) -> Value {
    let report = parse_json(stdout, label);
    if report["ok"] == json!(true) {
        return report["data"].clone();
    }
    report["error"]["details"].clone()
}

/// Export errors that are not about corpus composition.
fn conversion_errors(body: &Value) -> Vec<String> {
    body["errors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .filter(|error| !error.contains("behavior strata"))
        .map(str::to_string)
        .collect()
}

/// A minimal `harn.agent_training_example.v1` row.
///
/// `read_file` declares `start_line` and `end_line`, but the recorded call
/// passes only `path` — the case where inferring a schema from observed
/// arguments silently narrows the tool.
fn projected_example(messages: Value) -> Value {
    json!({
        "schema_version": "harn.agent_training_example.v1",
        "id": "projected-1",
        "messages": messages,
        "tools": [{
            "name": "read_file",
            "description": "Read a file from the workspace.",
            "compact": false,
            "params": [
                {
                    "name": "path",
                    "ty": {"Primitive": "string"},
                    "description": "File to read.",
                    "required": true,
                    "default": null,
                    "examples": [],
                },
                {
                    "name": "start_line",
                    "ty": {"Primitive": "integer"},
                    "description": "First line to include.",
                    "required": false,
                    "default": null,
                    "examples": [],
                },
                {
                    "name": "end_line",
                    "ty": {"Primitive": "integer"},
                    "description": "Last line to include.",
                    "required": false,
                    "default": null,
                    "examples": [],
                },
            ],
        }],
        "provenance": {
            "run_id": "run-projected",
            "session_id": "sess-projected",
            "provider": "llamacpp",
            "model": "qwen3.6-35b-a3b-ud-q4-k-xl",
            "effective_tool_format": "text",
            "tool_catalog_hash": "catalog-hash-1",
            "terminal_status": "completed",
            "usage": {"provider_calls": 2, "input_tokens": 120, "output_tokens": 40},
            "source": {
                "descriptor_schema_version": "harn.llm_transcript_artifact.v1",
                "transcript_path": "/runs/run-projected-llm/llm_transcript.jsonl",
                "transcript_sha256": "sha256:abc",
                "transcript_byte_len": 512,
                "event_count": 12,
                "first_event_index": 1,
                "last_event_index": 12,
                "first_event_id": "row:1",
                "last_event_id": "row:12",
            },
        },
    })
}

fn assistant_with_call() -> Value {
    json!({
        "role": "assistant",
        "content": "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"main.rs\"}}\n</tool_call>",
        "tool_calls": [{
            "id": "tc_0",
            "type": "function",
            "function": {"name": "read_file", "arguments": {"path": "main.rs"}},
        }],
    })
}

fn paired_messages() -> Value {
    json!([
        {"role": "system", "content": "You are a coding agent."},
        {"role": "user", "content": "read main.rs"},
        assistant_with_call(),
        {"role": "tool", "content": "fn main() {}", "tool_call_id": "tc_0", "name": "read_file"},
        {"role": "assistant", "content": "It is an empty main."},
    ])
}

fn write_corpus(row: &Value) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("corpus.jsonl"),
        format!("{}\n", serde_json::to_string(row).expect("row JSON")),
    )
    .expect("write corpus");
    tmp
}

fn export(
    corpus: &tempfile::TempDir,
    out: Option<&std::path::Path>,
) -> super::support::HarnCliOutput {
    let corpus_path = corpus.path().join("corpus.jsonl");
    let mut args = vec![
        "models".to_string(),
        "lora".to_string(),
        "export".to_string(),
        "--base".to_string(),
        "local-gemma4-e4b".to_string(),
        "--provider".to_string(),
        "vllm".to_string(),
        "--tool-format".to_string(),
        "native".to_string(),
        "--corpus".to_string(),
        corpus_path.display().to_string(),
        "--json".to_string(),
    ];
    match out {
        Some(path) => {
            args.push("--out".to_string());
            args.push(path.display().to_string());
        }
        None => args.push("--check".to_string()),
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&borrowed, &[])
}

#[test]
fn exports_a_projected_example_and_keeps_the_served_catalog_exactly() {
    let corpus = write_corpus(&projected_example(paired_messages()));
    let out = corpus.path().join("dataset.jsonl");
    let harn = export(&corpus, Some(&out));
    let data = report_body(&harn.stdout, "lora export");
    assert_eq!(conversion_errors(&data), Vec::<String>::new());
    assert_eq!(data["stats"]["emitted"], json!(1));
    assert_eq!(data["stats"]["skipped"], json!(0));
    assert_eq!(data["stats"]["tool_calls"], json!(1));
    assert_eq!(data["stats"]["tool_results"], json!(1));

    let written = fs::read_to_string(&out).expect("read dataset");
    let row: Value = serde_json::from_str(written.trim()).expect("dataset row");

    // The declared catalog reaches the trainer intact. A schema synthesized
    // from the one observed argument would carry `path` alone.
    let parameters = &row["tools"][0]["function"]["parameters"];
    assert_eq!(row["tools"][0]["function"]["name"], json!("read_file"));
    assert_eq!(parameters["required"], json!(["path"]));
    for declared in ["path", "start_line", "end_line"] {
        assert!(
            parameters["properties"].get(declared).is_some(),
            "declared parameter `{declared}` was dropped: {parameters}"
        );
    }

    // The typed pair survives; nothing was re-parsed out of text.
    let messages = row["messages"].as_array().expect("messages");
    let call = &messages[2]["tool_calls"][0];
    assert_eq!(call["id"], json!("tc_0"));
    assert_eq!(call["function"]["arguments"], json!({"path": "main.rs"}));
    assert_eq!(messages[3]["role"], json!("tool"));
    assert_eq!(messages[3]["tool_call_id"], json!("tc_0"));

    // Provenance rides along, so a trained adapter traces back to its run.
    assert_eq!(row["metadata"]["tool_catalog_source"], json!("projected"));
    assert_eq!(
        row["metadata"]["source_projection"]["run_id"],
        json!("run-projected")
    );
}

/// Every way a projected row can break the pairing invariant, and the refusal
/// it must produce. None of these may be exported.
#[test]
fn refuses_projected_examples_that_break_the_pairing_invariant() {
    let cases: [(&str, Value); 4] = [
        (
            "a generic placeholder standing in for a tool result",
            json!([
                {"role": "system", "content": "You are a coding agent."},
                assistant_with_call(),
                {"role": "user", "content": "[tool results applied; continuing]"},
                {"role": "assistant", "content": "done"},
            ]),
        ),
        (
            "a call with no result at all",
            json!([
                {"role": "system", "content": "You are a coding agent."},
                assistant_with_call(),
                {"role": "assistant", "content": "done"},
            ]),
        ),
        (
            "a result naming a call nobody made",
            json!([
                {"role": "system", "content": "You are a coding agent."},
                assistant_with_call(),
                {"role": "tool", "content": "x", "tool_call_id": "tc_9", "name": "read_file"},
                {"role": "assistant", "content": "done"},
            ]),
        ),
        (
            "the same call answered twice",
            json!([
                {"role": "system", "content": "You are a coding agent."},
                assistant_with_call(),
                {"role": "tool", "content": "x", "tool_call_id": "tc_0", "name": "read_file"},
                {"role": "tool", "content": "x", "tool_call_id": "tc_0", "name": "read_file"},
                {"role": "assistant", "content": "done"},
            ]),
        ),
    ];

    for (label, messages) in cases {
        let corpus = write_corpus(&projected_example(messages));
        let harn = export(&corpus, None);
        let body = report_body(&harn.stdout, "lora export");
        let errors = conversion_errors(&body);
        assert!(
            errors.iter().any(|error| {
                error.contains("unpaired_tool_call")
                    || error.contains("orphaned_tool_result")
                    || error.contains("duplicate_tool_call_id")
            }),
            "{label}: expected a pairing refusal, got {errors:?}"
        );
        // The offending row never reaches the dataset.
        assert_eq!(body["stats"]["emitted"], json!(0), "{label}");
        assert_eq!(body["stats"]["skipped"], json!(1), "{label}");
    }
}

#[test]
fn refuses_a_projected_example_whose_catalog_harn_cannot_render() {
    let mut row = projected_example(paired_messages());
    row["tools"] = json!([{"description": "no name here"}]);
    let corpus = write_corpus(&row);

    let harn = export(&corpus, None);
    let errors = conversion_errors(&report_body(&harn.stdout, "lora export"));
    assert!(
        errors.iter().any(|error| error.contains("cannot render")),
        "{errors:?}"
    );
}

/// A manually-curated legacy row — no Harn projection — whose tool result was
/// flattened into a generic placeholder.
///
/// This is the concrete loss case the projector exists to close. The
/// placeholder is not evidence that the call was answered, so it must fail
/// closed rather than be emitted as an ordinary `user` message beside an
/// assistant turn whose `tool_calls` then have no matching results.
#[test]
fn refuses_a_legacy_row_whose_result_is_a_generic_placeholder() {
    let row = json!({
        "id": "legacy-placeholder",
        "language": "rust",
        "task_type": "explain",
        "eval_name": "legacy-placeholder",
        "metadata": {"behavior_class": "valid_tool_call"},
        "messages": [
            {
                "role": "system",
                "content": "Available tools: read_file\ndeclare function read_file(args: { path: string }): string;",
            },
            {"role": "user", "content": "Read main.rs."},
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"read_file\",\"arguments\":{\"path\":\"main.rs\"}}\n</tool_call>\n",
            },
            {"role": "user", "content": "[tool results applied; continuing]"},
            {"role": "assistant", "content": "It is an empty main. ##DONE##"},
        ],
    });
    let corpus = write_corpus(&row);

    let harn = export(&corpus, None);
    let body = report_body(&harn.stdout, "lora export");
    let errors = conversion_errors(&body);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("typed tool-result messages")),
        "expected the placeholder to fail closed, got {errors:?}"
    );
    assert_eq!(body["stats"]["emitted"], json!(0));
    assert_eq!(body["stats"]["skipped"], json!(1));
}
