//! End-to-end coverage for `harn runs export-training`.
//!
//! Every run here comes out of the real agent loop via the public
//! `agent_loop` / `run_record_save` lifecycle, so the projector is tested
//! against what the runtime actually records rather than against a sidecar
//! hand-written to match it.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;

use crate::test_util::process::{run_harn_e2e, HarnCliOutput};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

struct Run {
    _temp: TempDir,
    dir: PathBuf,
    record: PathBuf,
}

/// Produce one real run on the requested tool-call channel.
fn agent_run(channel: &str, run_id: &str) -> Run {
    let temp = TempDir::new().expect("tempdir");
    let dir = temp.path().to_path_buf();
    let script = fixture("training_example_run.harn");
    let output = run_harn_e2e(
        &["run", script.to_str().expect("utf8 fixture path")],
        &[
            ("TRAINING_RUN_DIR", dir.to_str().expect("utf8 dir")),
            ("TRAINING_RUN_CHANNEL", channel),
            ("TRAINING_RUN_ID", run_id),
        ],
    );
    assert_eq!(
        output.exit_code, 0,
        "agent run failed:\n{}\n{}",
        output.stdout, output.stderr,
    );
    let record = dir.join(format!("{run_id}.json"));
    assert!(record.is_file(), "run record was not persisted");
    Run {
        _temp: temp,
        dir,
        record,
    }
}

fn export(args: &[&str]) -> HarnCliOutput {
    let mut argv = vec!["runs", "export-training"];
    argv.extend_from_slice(args);
    run_harn_e2e(&argv, &[])
}

fn export_ok(args: &[&str]) -> Value {
    let output = export(args);
    assert_eq!(
        output.exit_code, 0,
        "export failed:\n{}\n{}",
        output.stdout, output.stderr,
    );
    let envelope: Value = serde_json::from_str(output.stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not JSON: {error}\n{}", output.stdout));
    assert_eq!(envelope["ok"], Value::Bool(true), "{envelope}");
    envelope["data"]["example"].clone()
}

/// The structured refusal for an export that must not succeed.
fn export_error(args: &[&str]) -> Value {
    let output = export(args);
    assert_ne!(
        output.exit_code, 0,
        "export unexpectedly succeeded:\n{}",
        output.stdout,
    );
    let envelope: Value = serde_json::from_str(output.stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not JSON: {error}\n{}", output.stdout));
    assert_eq!(envelope["ok"], Value::Bool(false), "{envelope}");
    envelope["error"].clone()
}

fn roles(example: &Value) -> Vec<String> {
    example["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .map(|message| message["role"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `tool_schemas` catalog the run actually served, read straight from the
/// sidecar. The projected example must equal this exactly.
fn served_catalog(run: &Run, run_id: &str) -> Value {
    let sidecar = run.dir.join(format!("{run_id}-llm/llm_transcript.jsonl"));
    let text = std::fs::read_to_string(&sidecar).expect("read sidecar");
    for line in text.lines() {
        let event: Value = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) => continue,
        };
        if event["type"] == Value::String("tool_schemas".to_string()) {
            return event["schemas"].clone();
        }
    }
    panic!("run served no tool catalog");
}

fn tool_named<'a>(catalog: &'a Value, name: &str) -> &'a Value {
    catalog
        .as_array()
        .expect("catalog array")
        .iter()
        .find(|tool| tool["name"] == Value::String(name.to_string()))
        .unwrap_or_else(|| panic!("catalog has no `{name}`"))
}

#[test]
fn projects_a_native_channel_run_with_paired_calls_and_exact_provenance() {
    let run = agent_run("native", "run_native");
    let example = export_ok(&[run.record.to_str().unwrap(), "--json"]);

    assert_eq!(
        example["schema_version"],
        Value::String("harn.agent_training_example.v1".to_string())
    );
    assert_eq!(
        roles(&example),
        ["system", "user", "assistant", "tool", "assistant"]
    );

    let messages = example["messages"].as_array().unwrap();
    let call_id = messages[2]["tool_calls"][0]["id"]
        .as_str()
        .expect("assistant call id");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["name"],
        Value::String("read_file".to_string())
    );
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"]["path"],
        Value::String("main.rs".to_string())
    );
    // The result names the call it answers, so the pair survives export.
    assert_eq!(
        messages[3]["tool_call_id"],
        Value::String(call_id.to_string())
    );
    assert_eq!(messages[3]["name"], Value::String("read_file".to_string()));

    let provenance = &example["provenance"];
    assert_eq!(
        provenance["run_id"],
        Value::String("run_native".to_string())
    );
    assert!(provenance["session_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert_eq!(provenance["provider"], Value::String("mock".to_string()));
    assert_eq!(provenance["model"], Value::String("mock".to_string()));
    assert!(provenance["effective_tool_format"]
        .as_str()
        .is_some_and(|format| !format.is_empty()));
    assert!(provenance["tool_catalog_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));

    // Source identity is the run's own transcript, bound by digest.
    let source = &provenance["source"];
    let sidecar = run.dir.join("run_native-llm/llm_transcript.jsonl");
    assert_eq!(
        source["transcript_path"],
        Value::String(sidecar.to_string_lossy().into_owned())
    );
    assert_eq!(
        source["transcript_byte_len"].as_u64(),
        Some(std::fs::metadata(&sidecar).unwrap().len())
    );
    assert!(source["transcript_sha256"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));
    assert!(source["event_count"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn projects_a_text_channel_run_whose_result_carried_no_id() {
    let run = agent_run("text", "run_text");
    let example = export_ok(&[run.record.to_str().unwrap(), "--json"]);

    assert_eq!(
        roles(&example),
        ["system", "user", "assistant", "tool", "assistant"]
    );
    let messages = example["messages"].as_array().unwrap();

    // On this channel the served result was an ordinary `user` echo with no
    // id on it. The projection still recovers the pair, from the dispatch and
    // result receipts, and normalizes it to the canonical `tool` role.
    let call_id = messages[2]["tool_calls"][0]["id"]
        .as_str()
        .expect("call id");
    assert_eq!(messages[3]["role"], Value::String("tool".to_string()));
    assert_eq!(
        messages[3]["tool_call_id"],
        Value::String(call_id.to_string())
    );

    // The assistant's served text is preserved byte-exact, so a text-format
    // target can be rendered without re-synthesizing the tagged block.
    assert!(
        messages[2]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("<tool_call>"),
        "assistant text was not preserved: {}",
        messages[2]["content"]
    );
    assert_eq!(
        example["provenance"]["effective_tool_format"],
        Value::String("text".to_string())
    );
}

#[test]
fn carries_the_served_catalog_through_instead_of_inferring_one() {
    let run = agent_run("native", "run_catalog");
    let example = export_ok(&[run.record.to_str().unwrap(), "--json"]);

    // Exact equality with what the model was served — not a superset, not a
    // reconstruction.
    assert_eq!(example["tools"], served_catalog(&run, "run_catalog"));

    // The run only ever passed `path`. A catalog inferred from observed
    // argument values would drop the two parameters it never used, teaching a
    // narrower tool than the model actually saw.
    let declared = tool_named(&example["tools"], "read_file");
    let params: Vec<&str> = declared["params"]
        .as_array()
        .expect("declared params")
        .iter()
        .filter_map(|param| param["name"].as_str())
        .collect();
    assert!(params.contains(&"path"), "{params:?}");
    assert!(params.contains(&"start_line"), "{params:?}");
    assert!(params.contains(&"end_line"), "{params:?}");
}

#[test]
fn writing_the_example_out_reproduces_the_reported_projection() {
    let run = agent_run("native", "run_out");
    let out = run.dir.join("example.jsonl");
    let example = export_ok(&[
        run.record.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--json",
    ]);

    let written = std::fs::read_to_string(&out).expect("read written example");
    let row: Value = serde_json::from_str(written.trim()).expect("written row is JSON");
    assert_eq!(row, example);
}

#[test]
fn asserting_the_right_run_id_projects_and_a_wrong_one_refuses() {
    let run = agent_run("native", "run_pinned");
    let example = export_ok(&[
        run.record.to_str().unwrap(),
        "--run-id",
        "run_pinned",
        "--json",
    ]);
    assert_eq!(
        example["provenance"]["run_id"],
        Value::String("run_pinned".to_string())
    );

    let error = export_error(&[
        run.record.to_str().unwrap(),
        "--run-id",
        "run_somebody_else",
        "--json",
    ]);
    assert_eq!(error["code"], Value::String("run_id_mismatch".to_string()));
}

#[test]
fn asserting_a_foreign_session_id_refuses() {
    let run = agent_run("native", "run_session");
    let error = export_error(&[
        run.record.to_str().unwrap(),
        "--session-id",
        "agent_session_not_this_one",
        "--json",
    ]);
    assert_eq!(
        error["code"],
        Value::String("session_id_mismatch".to_string())
    );
}

#[test]
fn a_directory_of_several_runs_needs_an_explicit_authority_id() {
    // Two real runs side by side, deliberately differing in output length so a
    // "longest wins" or "has a DONE marker" heuristic would have something to
    // latch onto.
    let first = agent_run("native", "run_a");
    let second = agent_run("text", "run_b");
    let shared = TempDir::new().expect("tempdir");
    for run in [(&first, "run_a"), (&second, "run_b")] {
        copy_run(run.0, run.1, shared.path());
    }

    let error = export_error(&[shared.path().to_str().unwrap(), "--json"]);
    assert_eq!(
        error["code"],
        Value::String("ambiguous_authority".to_string())
    );

    let example = export_ok(&[
        shared.path().to_str().unwrap(),
        "--run-id",
        "run_b",
        "--json",
    ]);
    assert_eq!(
        example["provenance"]["run_id"],
        Value::String("run_b".to_string())
    );
}

fn copy_run(run: &Run, run_id: &str, into: &Path) {
    let sidecar_dir = into.join(format!("{run_id}-llm"));
    std::fs::create_dir_all(&sidecar_dir).expect("create sidecar dir");
    std::fs::copy(
        run.dir.join(format!("{run_id}-llm/llm_transcript.jsonl")),
        sidecar_dir.join("llm_transcript.jsonl"),
    )
    .expect("copy sidecar");
    std::fs::copy(&run.record, into.join(format!("{run_id}.json"))).expect("copy run record");
}

/// Rewrite the sidecar and report the structured refusal that follows.
fn refusal_after_sidecar_edit(run_id: &str, edit: impl Fn(&str) -> String) -> Value {
    let run = agent_run("native", run_id);
    let sidecar = run.dir.join(format!("{run_id}-llm/llm_transcript.jsonl"));
    let body = std::fs::read_to_string(&sidecar).expect("read sidecar");
    std::fs::write(&sidecar, edit(&body)).expect("rewrite sidecar");
    export_error(&[run.record.to_str().unwrap(), "--json"])
}

#[test]
fn a_sidecar_edited_after_the_run_finalized_refuses() {
    let error = refusal_after_sidecar_edit("run_mutated", |body| format!("{body} \n"));
    assert_eq!(error["code"], Value::String("digest_mismatch".to_string()));
}

#[test]
fn a_malformed_row_refuses_instead_of_being_skipped() {
    let error = refusal_after_sidecar_edit("run_malformed", |body| format!("{body}{{not json}}\n"));
    // The bytes no longer match the descriptor written at save time, and the
    // row cannot be parsed either; both layers refuse rather than dropping it.
    assert!(
        matches!(
            error["code"].as_str(),
            Some("digest_mismatch" | "malformed_jsonl")
        ),
        "{error}"
    );
}

#[test]
fn a_run_with_no_terminal_record_refuses_as_incomplete() {
    let error = refusal_after_sidecar_edit("run_truncated", |body| {
        body.lines()
            .filter(|line| !line.contains("agent_session_finalized"))
            .map(|line| format!("{line}\n"))
            .collect()
    });
    assert!(
        matches!(
            error["code"].as_str(),
            Some("incomplete_source" | "digest_mismatch")
        ),
        "{error}"
    );
}

#[test]
fn a_missing_run_record_refuses() {
    let error = export_error(&["/definitely/not/a/run.json", "--json"]);
    assert_eq!(
        error["code"],
        Value::String("run_record_unreadable".to_string())
    );
}
