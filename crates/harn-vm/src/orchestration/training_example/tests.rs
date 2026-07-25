//! Unit coverage for the projection state machine.
//!
//! These drive the projector over hand-built sidecars so each structured
//! failure has an isolated cause. End-to-end coverage over a real agent run
//! and the public CLI lives in `harn-cli`'s integration suite.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::orchestration::{
    describe_llm_transcript_sidecar, RunObservabilityRecord, RunRecord, RunTranscriptPointerRecord,
};

struct Artifact {
    _temp: tempfile::TempDir,
    run_path: PathBuf,
}

/// Write a run record whose persisted descriptor really binds the sidecar, the
/// same way run persistence does.
fn artifact(run_id: &str, rows: &[serde_json::Value]) -> Artifact {
    artifact_with_bytes(
        run_id,
        &rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
}

fn artifact_with_bytes(run_id: &str, body: &str) -> Artifact {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    let transcript_path = temp.path().join("run-llm/llm_transcript.jsonl");
    std::fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
    std::fs::write(&transcript_path, body).unwrap();
    let mut run = RunRecord {
        id: run_id.to_string(),
        ..Default::default()
    };
    let descriptor = describe_llm_transcript_sidecar(&run, &run_path, &transcript_path).ok();
    run.observability = Some(RunObservabilityRecord {
        transcript_pointers: vec![RunTranscriptPointerRecord {
            id: "run:llm_transcript".to_string(),
            kind: "llm_jsonl".to_string(),
            path: Some(transcript_path.to_string_lossy().into_owned()),
            available: descriptor.is_some(),
            verification_status: "verified".to_string(),
            descriptor,
            ..Default::default()
        }],
        ..Default::default()
    });
    std::fs::write(&run_path, serde_json::to_string_pretty(&run).unwrap()).unwrap();
    Artifact {
        _temp: temp,
        run_path,
    }
}

fn project_path(path: &Path) -> Result<AgentTrainingExample, TrainingExampleError> {
    project_agent_training_example(&TrainingExampleRequest::new(path))
}

fn tool_catalog() -> serde_json::Value {
    json!({
        "type": "tool_schemas",
        "hash": "schema-hash-1",
        "content_hash": "blake3:catalog",
        "schemas": [{
            "name": "read_file",
            "description": "Read a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"},
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        }],
    })
}

fn system_prompt() -> serde_json::Value {
    json!({"type": "system_prompt", "hash": 1, "content": "You are a coding agent."})
}

fn request(call_id: &str, tool_format: &str) -> serde_json::Value {
    json!({
        "type": "provider_call_request",
        "call_id": call_id,
        "iteration": 0,
        "model": "qwen3.6-35b",
        "provider": "llamacpp",
        "tool_format": tool_format,
        "route_policy": "primary",
    })
}

fn response(call_id: &str, text: &str, parsed: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "provider_call_response",
        "call_id": call_id,
        "provider": "llamacpp",
        "model": "qwen3.6-35b",
        "text": text,
        "tool_calls": [],
        "parsed_tool_calls": parsed,
        "input_tokens": 120,
        "output_tokens": 40,
    })
}

fn message(index: u64, role: &str, body: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "message",
        "session_id": "sess-1",
        "message_index": index,
        "role": role,
        "content": body.get("content").cloned().unwrap_or(serde_json::Value::Null),
        "message": body,
    })
}

fn receipt(index: u64, call_id: &str, tool_name: &str) -> serde_json::Value {
    json!({
        "type": "tool_result",
        "schema_version": TOOL_RESULT_RECEIPT_VERSION,
        "session_id": "sess-1",
        "message_index": index,
        "call_id": call_id,
        "tool_name": tool_name,
        "ok": true,
        "tool_format": "text",
    })
}

/// Dispatch receipt: the id the call actually ran under.
fn dispatched(
    assistant_index: u64,
    call_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    json!({
        "type": "tool_call",
        "schema_version": TOOL_CALL_RECEIPT_VERSION,
        "session_id": "sess-1",
        "assistant_message_index": assistant_index,
        "call_id": call_id,
        "tool_name": tool_name,
        "arguments": arguments,
    })
}

fn terminal() -> serde_json::Value {
    json!({"type": "agent_session_finalized", "final_status": "completed", "terminal": true})
}

/// A text-channel run: the tool result rides back as a `user` echo carrying no
/// id, so only the receipt can pair it.
fn text_channel_rows() -> Vec<serde_json::Value> {
    vec![
        system_prompt(),
        tool_catalog(),
        message(
            0,
            "user",
            json!({"role": "user", "content": "read main.rs"}),
        ),
        request("call-1", "text"),
        response(
            "call-1",
            "<tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"main.rs\"}}</tool_call>",
            json!([{"id": "tc_0", "name": "read_file", "arguments": {"path": "main.rs"}}]),
        ),
        message(
            1,
            "assistant",
            json!({
                "role": "assistant",
                "content": "<tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"main.rs\"}}</tool_call>",
            }),
        ),
        dispatched(1, "tc_0", "read_file", json!({"path": "main.rs"})),
        receipt(2, "tc_0", "read_file"),
        message(
            2,
            "user",
            json!({"role": "user", "content": "fn main() {}"}),
        ),
        request("call-2", "text"),
        response("call-2", "done", json!([])),
        message(
            3,
            "assistant",
            json!({"role": "assistant", "content": "done"}),
        ),
        terminal(),
    ]
}

#[test]
fn projects_a_text_channel_run_into_canonical_messages() {
    let artifact = artifact("run-1", &text_channel_rows());
    let example = project_path(&artifact.run_path).unwrap();

    assert_eq!(example.schema_version, TRAINING_EXAMPLE_SCHEMA_VERSION);
    let roles = example
        .messages
        .iter()
        .map(|message| message.role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec!["system", "user", "assistant", "tool", "assistant"]
    );

    // The text-channel result is normalized to the canonical `tool` role and
    // carries the call it answers, which the served message never did.
    let result = &example.messages[3];
    assert_eq!(result.tool_call_id.as_deref(), Some("tc_0"));
    assert_eq!(result.name.as_deref(), Some("read_file"));
    assert_eq!(result.content, "fn main() {}");

    // The assistant keeps its verbatim served text *and* gains typed calls.
    let assistant = &example.messages[2];
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(assistant.tool_calls[0].id, "tc_0");
    assert_eq!(assistant.tool_calls[0].function.name, "read_file");
    assert!(assistant.content.contains("<tool_call>"));

    assert_eq!(example.provenance.effective_tool_format, "text");
    // The session claimed the text channel and served its result there; the
    // per-call effective format agrees here, but they are recorded apart.
    assert_eq!(
        example.provenance.declared_tool_format.as_deref(),
        Some("text")
    );
    assert_eq!(example.provenance.provider, "llamacpp");
    assert_eq!(example.provenance.model, "qwen3.6-35b");
    assert_eq!(example.provenance.session_id, "sess-1");
    assert_eq!(example.provenance.tool_catalog_hash, "schema-hash-1");
    assert_eq!(example.provenance.terminal_status.as_str(), "completed");
    assert_eq!(example.provenance.usage.provider_calls, 2);
    assert_eq!(example.provenance.usage.input_tokens, 240);
    assert!(example
        .provenance
        .source
        .transcript_sha256
        .starts_with("sha256:"));
}

#[test]
fn preserves_the_served_catalog_rather_than_inferring_one() {
    // The call supplies only `path`; `start_line`/`end_line` are declared but
    // unused. Inference from observed arguments would drop them and would
    // synthesize a permissive `additionalProperties: true` object.
    let artifact = artifact("run-1", &text_channel_rows());
    let example = project_path(&artifact.run_path).unwrap();

    let declared = tool_catalog();
    let expected = declared.get("schemas").unwrap().as_array().unwrap();
    assert_eq!(&example.tools, expected);
    let parameters = &example.tools[0]["parameters"];
    assert!(parameters["properties"].get("start_line").is_some());
    assert_eq!(parameters["additionalProperties"], json!(false));
}

#[test]
fn projects_a_native_channel_run() {
    let native_call = json!({
        "id": "call_abc",
        "type": "function",
        "function": {"name": "read_file", "arguments": "{\"path\":\"main.rs\"}"},
    });
    let rows = vec![
        system_prompt(),
        tool_catalog(),
        message(
            0,
            "user",
            json!({"role": "user", "content": "read main.rs"}),
        ),
        request("call-1", "native"),
        json!({
            "type": "provider_call_response",
            "call_id": "call-1",
            "provider": "anthropic",
            "model": "claude-opus-5",
            "text": "",
            "tool_calls": [native_call],
            "parsed_tool_calls": [native_call],
            "input_tokens": 10,
            "output_tokens": 5,
        }),
        message(
            1,
            "assistant",
            json!({"role": "assistant", "content": "", "tool_calls": [native_call]}),
        ),
        dispatched(1, "call_abc", "read_file", json!({"path": "main.rs"})),
        receipt(2, "call_abc", "read_file"),
        message(
            2,
            "tool",
            json!({
                "role": "tool",
                "name": "read_file",
                "tool_call_id": "call_abc",
                "content": "fn main() {}",
            }),
        ),
        request("call-2", "native"),
        json!({
            "type": "provider_call_response",
            "call_id": "call-2",
            "provider": "anthropic",
            "model": "claude-opus-5",
            "text": "done",
            "tool_calls": [],
            "parsed_tool_calls": [],
            "input_tokens": 10,
            "output_tokens": 5,
        }),
        message(
            3,
            "assistant",
            json!({"role": "assistant", "content": "done"}),
        ),
        terminal(),
    ];
    let artifact = artifact("run-1", &rows);
    let example = project_path(&artifact.run_path).unwrap();

    let assistant = &example.messages[2];
    assert_eq!(assistant.tool_calls[0].id, "call_abc");
    // String-encoded provider arguments are decoded once, here, so no consumer
    // needs a second wire parser.
    assert_eq!(
        assistant.tool_calls[0].function.arguments,
        json!({"path": "main.rs"})
    );
    assert_eq!(
        example.messages[3].tool_call_id.as_deref(),
        Some("call_abc")
    );
    assert_eq!(example.provenance.effective_tool_format, "native");
}

/// Locate a row by shape rather than by ordinal, so adding an event to the
/// canonical stream does not silently retarget every failure fixture.
fn row_at(rows: &[serde_json::Value], type_name: &str, nth: usize) -> usize {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row["type"] == json!(type_name))
        .map(|(index, _)| index)
        .nth(nth)
        .unwrap_or_else(|| panic!("no {type_name} row #{nth}"))
}

fn expect_failure(rows: &[serde_json::Value], kind: &str) -> TrainingExampleError {
    let artifact = artifact("run-1", rows);
    let error = project_path(&artifact.run_path).unwrap_err();
    assert_eq!(error.kind, kind, "unexpected error: {error}");
    error
}

#[test]
fn rejects_malformed_jsonl_instead_of_skipping_the_row() {
    // Strictness is checked at the projector's own loader, not borrowed from
    // the descriptor layer. Both are strict today; if the descriptor ever
    // relaxes, a dropped row must still be impossible here.
    let mut body = text_channel_rows()
        .iter()
        .take(3)
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    body.push_str("{not json}\n");

    let error = super::source::parse_events(&body, "sidecar").unwrap_err();
    assert_eq!(error.kind, "malformed_jsonl");
    assert_eq!(error.event_index, Some(4));
}

#[test]
fn rejects_a_row_with_no_type_discriminator() {
    let error = super::source::parse_events("{\"content\":\"x\"}\n", "sidecar").unwrap_err();
    assert_eq!(error.kind, "malformed_jsonl");
    assert!(error.message.contains("`type`"), "{error}");
}

#[test]
fn a_malformed_sidecar_never_produces_an_example() {
    let mut body = text_channel_rows()
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    body.push_str("{not json}\n");
    let artifact = artifact_with_bytes("run-1", &body);
    let error = project_path(&artifact.run_path).unwrap_err();
    // Run persistence refuses to bless the artifact at all, so the run carries
    // no descriptor for an authoritative consumer to trust.
    assert_eq!(error.kind, "missing_descriptor");
}

#[test]
fn rejects_a_missing_tool_result() {
    let mut rows = text_channel_rows();
    // Drop the result message and its receipt; the call is never answered.
    rows.remove(row_at(&rows, "message", 2));
    rows.remove(row_at(&rows, "tool_result", 0));
    let error = expect_failure(&rows, "unpaired_tool_call");
    assert!(error.message.contains("tc_0"), "{error}");
}

#[test]
fn rejects_an_orphaned_tool_result() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "tool_result", 0);
    rows[at] = receipt(2, "tc_9", "read_file");
    expect_failure(&rows, "orphaned_tool_result");
}

#[test]
fn rejects_two_receipts_claiming_one_message() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "tool_result", 0);
    rows.insert(at + 1, receipt(2, "tc_0", "read_file"));
    expect_failure(&rows, "duplicate_tool_result_receipt");
}

#[test]
fn rejects_a_second_result_for_an_already_answered_call() {
    let mut rows = text_channel_rows();
    // A distinct message, with its own receipt, answering the call that the
    // message before it already closed.
    let last = row_at(&rows, "message", 3);
    rows[last] = message(
        4,
        "assistant",
        json!({"role": "assistant", "content": "done"}),
    );
    let after_result = row_at(&rows, "message", 2) + 1;
    rows.insert(after_result, receipt(3, "tc_0", "read_file"));
    rows.insert(
        after_result + 1,
        message(
            3,
            "user",
            json!({"role": "user", "content": "fn main() {} again"}),
        ),
    );
    expect_failure(&rows, "orphaned_tool_result");
}

#[test]
fn rejects_out_of_order_tool_results() {
    let two_calls = json!([
        {"id": "tc_0", "name": "read_file", "arguments": {"path": "a.rs"}},
        {"id": "tc_1", "name": "read_file", "arguments": {"path": "b.rs"}},
    ]);
    let rows = vec![
        system_prompt(),
        tool_catalog(),
        message(0, "user", json!({"role": "user", "content": "read both"})),
        request("call-1", "text"),
        response("call-1", "calling", two_calls),
        message(
            1,
            "assistant",
            json!({"role": "assistant", "content": "calling"}),
        ),
        dispatched(1, "tc_0", "read_file", json!({"path": "a.rs"})),
        dispatched(1, "tc_1", "read_file", json!({"path": "b.rs"})),
        receipt(2, "tc_1", "read_file"),
        message(2, "user", json!({"role": "user", "content": "b"})),
        receipt(3, "tc_0", "read_file"),
        message(3, "user", json!({"role": "user", "content": "a"})),
        terminal(),
    ];
    expect_failure(&rows, "out_of_order_tool_result");
}

#[test]
fn rejects_an_unknown_required_event_version() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "tool_result", 0);
    rows[at] = json!({
        "type": "tool_result",
        "schema_version": "harn.llm_tool_result_receipt.v99",
        "session_id": "sess-1",
        "message_index": 2,
        "call_id": "tc_0",
        "tool_name": "read_file",
    });
    expect_failure(&rows, "unknown_event_version");
}

#[test]
fn rejects_a_truncated_terminal_record() {
    let mut rows = text_channel_rows();
    rows.pop();
    expect_failure(&rows, "incomplete_source");
}

#[test]
fn rejects_a_wrong_run_identity() {
    let artifact = artifact("run-1", &text_channel_rows());
    let mut request = TrainingExampleRequest::new(&artifact.run_path);
    request.run_id = Some("run-other".to_string());
    let error = project_agent_training_example(&request).unwrap_err();
    assert_eq!(error.kind, "run_id_mismatch");
}

#[test]
fn rejects_a_wrong_session_identity() {
    let artifact = artifact("run-1", &text_channel_rows());
    let mut request = TrainingExampleRequest::new(&artifact.run_path);
    request.session_id = Some("sess-other".to_string());
    let error = project_agent_training_example(&request).unwrap_err();
    assert_eq!(error.kind, "session_id_mismatch");
}

#[test]
fn rejects_a_sidecar_mutated_after_the_descriptor_was_written() {
    let artifact = artifact("run-1", &text_channel_rows());
    let transcript = artifact
        .run_path
        .parent()
        .unwrap()
        .join("run-llm/llm_transcript.jsonl");
    let mut body = std::fs::read_to_string(&transcript).unwrap();
    body.push_str(&format!("{}\n", terminal()));
    std::fs::write(&transcript, body).unwrap();

    let error = project_path(&artifact.run_path).unwrap_err();
    assert_eq!(error.kind, "digest_mismatch");
}

#[test]
fn rejects_a_second_training_context_in_one_artifact() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "provider_call_request", 1);
    rows.insert(
        at,
        json!({"type": "system_prompt", "hash": 2, "content": "You are a different agent."}),
    );
    expect_failure(&rows, "system_prompt_changed");
}

#[test]
fn rejects_a_run_with_no_served_tool_catalog() {
    let mut rows = text_channel_rows();
    rows.remove(1);
    expect_failure(&rows, "missing_tool_catalog");
}

#[test]
fn rejects_a_call_outside_the_served_catalog() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "provider_call_response", 0);
    rows[at] = response(
        "call-1",
        "calling",
        json!([{"id": "tc_0", "name": "delete_everything", "arguments": {}}]),
    );
    let dispatch_at = row_at(&rows, "tool_call", 0);
    rows[dispatch_at] = dispatched(1, "tc_0", "delete_everything", json!({}));
    expect_failure(&rows, "tool_outside_catalog");
}

#[test]
fn rejects_a_receipt_that_names_the_wrong_message() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "tool_result", 0);
    rows[at] = receipt(99, "tc_0", "read_file");
    expect_failure(&rows, "unbound_tool_result_receipt");
}

#[test]
fn rejects_a_tool_result_with_no_receipt() {
    let mut rows = text_channel_rows();
    rows.remove(row_at(&rows, "tool_result", 0));
    let at = row_at(&rows, "message", 2);
    rows[at] = message(
        2,
        "tool",
        json!({"role": "tool", "tool_call_id": "tc_0", "content": "fn main() {}"}),
    );
    expect_failure(&rows, "orphaned_tool_result");
}

#[test]
fn rejects_a_receipt_disagreeing_with_a_native_result_id() {
    let mut rows = text_channel_rows();
    let at = row_at(&rows, "message", 2);
    rows[at] = message(
        2,
        "user",
        json!({"role": "user", "content": "fn main() {}", "tool_call_id": "tc_7"}),
    );
    expect_failure(&rows, "tool_result_identity_mismatch");
}

#[test]
fn pairing_validator_rejects_a_generic_placeholder_turn() {
    let messages = vec![
        TrainingMessage {
            role: "assistant".to_string(),
            content: "calling".to_string(),
            tool_calls: vec![TrainingToolCall {
                id: "tc_0".to_string(),
                call_type: "function".to_string(),
                function: TrainingToolFunction {
                    name: "read_file".to_string(),
                    arguments: json!({}),
                },
            }],
            ..TrainingMessage::default()
        },
        TrainingMessage {
            role: "user".to_string(),
            content: "[tool results applied; continuing]".to_string(),
            ..TrainingMessage::default()
        },
    ];
    let error = validate_training_example_pairing(&messages).unwrap_err();
    assert_eq!(error.kind, "unpaired_tool_call");
    assert!(error.message.contains("placeholder"), "{error}");
}

#[test]
fn pairing_validator_accepts_ordered_pairs() {
    let messages = vec![
        TrainingMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: vec![
                TrainingToolCall {
                    id: "tc_0".to_string(),
                    call_type: "function".to_string(),
                    function: TrainingToolFunction {
                        name: "read_file".to_string(),
                        arguments: json!({}),
                    },
                },
                TrainingToolCall {
                    id: "tc_1".to_string(),
                    call_type: "function".to_string(),
                    function: TrainingToolFunction {
                        name: "read_file".to_string(),
                        arguments: json!({}),
                    },
                },
            ],
            ..TrainingMessage::default()
        },
        TrainingMessage {
            role: "tool".to_string(),
            content: "a".to_string(),
            tool_call_id: Some("tc_0".to_string()),
            ..TrainingMessage::default()
        },
        TrainingMessage {
            role: "tool".to_string(),
            content: "b".to_string(),
            tool_call_id: Some("tc_1".to_string()),
            ..TrainingMessage::default()
        },
    ];
    validate_training_example_pairing(&messages).unwrap();
}
