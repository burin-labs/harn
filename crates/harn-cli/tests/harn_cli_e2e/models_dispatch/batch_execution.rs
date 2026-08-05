use std::fs;
use std::process::{Command, Stdio};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::support::{harn_e2e_binary, parse_json, run, success_data};

const REQUESTS: &str = r#"{"custom_id":"wire-anthropic","provider":"anthropic","model":"claude-3-5-haiku-20241022","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"wire-bedrock","provider":"bedrock","model":"anthropic.claude-sonnet-4-5-20250929-v1:0","provider_policy":{"region":"us-west-2","role_arn":"arn:aws:iam::123456789012:role/HarnBatch","input_s3_uri":"s3://fixture-bucket/input/requests.jsonl","output_s3_uri":"s3://fixture-bucket/output/results.jsonl"},"messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"wire-fireworks","provider":"fireworks","model":"accounts/fireworks/models/deepseek-v4-pro","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"wire-gemini","provider":"gemini","model":"gemini-2.5-flash","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"wire-mistral","provider":"mistral","model":"codestral-2508","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"wire-openai","provider":"groq","model":"groq/compound","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"wire-xai","provider":"xai","model":"grok-4.3","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
"#;

fn initialize(requests: &std::path::Path, execution: &std::path::Path) -> Value {
    fs::write(requests, REQUESTS).expect("write batch requests");
    let output = run(
        &[
            "models",
            "batch",
            "execute",
            "init",
            "--requests",
            requests.to_str().expect("utf8 requests"),
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(output.exit_code, 0, "init stderr={}", output.stderr);
    parse_json(&output.stdout, "batch execute init")
}

fn advance(execution: &std::path::Path) -> Value {
    let output = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_eq!(
        output.exit_code, 0,
        "advance stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    parse_json(&output.stdout, "batch execute advance")
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn write_json(path: &std::path::Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_string_pretty(value).expect("serialize JSON") + "\n",
    )
    .expect("write JSON");
}

fn result_artifact(download: &Value) -> (&str, &str) {
    download["jobs"]
        .as_array()
        .expect("download jobs")
        .iter()
        .flat_map(|job| {
            job["artifacts"]
                .as_array()
                .expect("download artifacts")
                .iter()
        })
        .find_map(|artifact| {
            let label = artifact["label"].as_str()?;
            matches!(label, "output" | "results" | "responses")
                .then(|| (label, artifact["path"].as_str().expect("artifact path")))
        })
        .expect("result artifact")
}

fn bind_mutated_download(
    execution_dir: &std::path::Path,
    download: &mut Value,
    execution: &mut Value,
    artifact_path: &str,
    artifact_bytes: &[u8],
) {
    fs::write(artifact_path, artifact_bytes).expect("write mutated result");
    let artifact_sha = sha256(artifact_bytes);
    for job in download["jobs"].as_array_mut().expect("download jobs") {
        for artifact in job["artifacts"].as_array_mut().expect("download artifacts") {
            if artifact["path"] == artifact_path {
                artifact["sha256"] = Value::String(artifact_sha.clone());
                artifact["bytes"] = Value::from(artifact_bytes.len());
            }
        }
    }
    for artifact in execution["artifacts"]
        .as_array_mut()
        .expect("execution artifacts")
    {
        if artifact["path"] == artifact_path {
            artifact["sha256"] = Value::String(artifact_sha.clone());
        }
    }
    let download_path = execution_dir.join("results/receipt.json");
    write_json(&download_path, download);
    let download_sha = sha256(&fs::read(&download_path).expect("read rebound download"));
    for artifact in execution["artifacts"]
        .as_array_mut()
        .expect("execution artifacts")
    {
        if artifact["role"] == "download_receipt" {
            artifact["sha256"] = Value::String(download_sha.clone());
        }
    }
    write_json(&execution_dir.join("execution.json"), execution);
}

fn run_rejoin(execution_dir: &std::path::Path, manifest: &std::path::Path, case: &str) -> Value {
    let out_dir = execution_dir.join(format!("rejoin-{case}"));
    let output = run(
        &[
            "models",
            "batch",
            "rejoin",
            "--execution",
            execution_dir
                .join("execution.json")
                .to_str()
                .expect("utf8 execution receipt"),
            "--manifest",
            manifest.to_str().expect("utf8 manifest"),
            "--download",
            execution_dir
                .join("results/receipt.json")
                .to_str()
                .expect("utf8 download"),
            "--out-dir",
            out_dir.to_str().expect("utf8 rejoin output"),
            "--json",
        ],
        &[],
    );
    assert_eq!(
        output.exit_code, 0,
        "rejoin case {case} stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    serde_json::from_slice(
        &fs::read(out_dir.join("receipt.json")).expect("read quarantine receipt"),
    )
    .expect("parse quarantine receipt")
}

#[test]
fn durable_fixture_execution_rejoins_every_supported_wire_family() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    let initialized = initialize(&requests, &execution);
    assert_eq!(success_data(&initialized)["phase"], "prepared");
    let prepare: Value = serde_json::from_slice(
        &fs::read(execution.join("prepared/receipt.json")).expect("read prepare receipt"),
    )
    .expect("parse prepare receipt");
    let jobs = prepare["jobs"].as_array().expect("prepare jobs");
    assert!(
        jobs.iter().any(|job| {
            job["provider"] == "bedrock" && job["create_recovery"]["mode"] == "deterministic_token"
        }),
        "jobs={jobs:?}"
    );
    assert!(jobs
        .iter()
        .filter(|job| job["provider"] != "bedrock")
        .all(|job| {
            job["create_recovery"]
                == serde_json::json!({
                    "mode": "reconcile_only",
                    "retry_after_ambiguous_acceptance": false
                })
        }));

    assert_eq!(success_data(&advance(&execution))["phase"], "submitted");
    assert_eq!(success_data(&advance(&execution))["phase"], "completed");
    assert_eq!(success_data(&advance(&execution))["phase"], "downloaded");
    let terminal = advance(&execution);
    let state = success_data(&terminal);
    assert_eq!(state["phase"], "rejoined");
    assert_eq!(state["consumable"], true);

    let receipt: Value = serde_json::from_slice(
        &fs::read(execution.join("rejoin/receipt.json")).expect("read rejoin receipt"),
    )
    .expect("parse rejoin receipt");
    assert_eq!(receipt["kind"], "harn.model_batch_rejoin_receipt");
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["consumable"], true);
    assert_eq!(receipt["matchedCount"], 7);
    assert_eq!(receipt["quarantine"]["reasons"], serde_json::json!([]));

    let normalized = fs::read_to_string(execution.join("rejoin/normalized.jsonl"))
        .expect("read normalized rows");
    let rows: Vec<Value> = normalized
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse normalized row"))
        .collect();
    let ids: Vec<_> = rows
        .iter()
        .map(|row| row["custom_id"].as_str().expect("custom_id"))
        .collect();
    assert_eq!(
        ids,
        [
            "wire-anthropic",
            "wire-bedrock",
            "wire-fireworks",
            "wire-gemini",
            "wire-mistral",
            "wire-openai",
            "wire-xai",
        ]
    );
    assert!(rows.iter().all(|row| row["state"] == "succeeded"));
    assert!(receipt["rawArtifacts"]
        .as_array()
        .is_some_and(|artifacts| artifacts.len() == 7));
}

#[test]
fn rejoin_quarantines_each_non_consumable_result_shape() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution_dir = tmp.path().join("execution");
    initialize(&requests, &execution_dir);
    advance(&execution_dir);
    advance(&execution_dir);
    assert_eq!(
        success_data(&advance(&execution_dir))["phase"],
        "downloaded"
    );

    let manifest_path = execution_dir.join("manifest.json");
    let download_path = execution_dir.join("results/receipt.json");
    let execution_path = execution_dir.join("execution.json");
    let base_download: Value =
        serde_json::from_slice(&fs::read(&download_path).expect("read download"))
            .expect("parse download");
    let base_execution: Value =
        serde_json::from_slice(&fs::read(&execution_path).expect("read execution"))
            .expect("parse execution");
    let (_, artifact_path) = result_artifact(&base_download);
    let base_artifact = fs::read(artifact_path).expect("read result artifact");
    let first_line = base_artifact
        .split(|byte| *byte == b'\n')
        .find(|line| !line.is_empty())
        .expect("first result row");
    let first_row: Value = serde_json::from_slice(first_line).expect("parse first result row");

    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("missing", Vec::new(), "missing_results"),
        (
            "duplicate",
            [base_artifact.clone(), first_line.to_vec(), vec![b'\n']].concat(),
            "duplicate_results",
        ),
        (
            "unexpected",
            [
                base_artifact.clone(),
                serde_json::to_vec(&serde_json::json!({
                    "custom_id": "wire-unexpected",
                    "response": {"status_code": 200, "body": {}}
                }))
                .expect("serialize unexpected"),
                vec![b'\n'],
            ]
            .concat(),
            "unexpected_results",
        ),
        (
            "idless",
            [
                base_artifact.clone(),
                serde_json::to_vec(&serde_json::json!({
                    "response": {"status_code": 200, "body": {}}
                }))
                .expect("serialize idless"),
                vec![b'\n'],
            ]
            .concat(),
            "rows_without_custom_id",
        ),
        (
            "malformed",
            [base_artifact.clone(), b"{not-json}\n".to_vec()].concat(),
            "malformed_results",
        ),
        (
            "provider-error",
            [
                serde_json::to_vec(&serde_json::json!({
                    "custom_id": first_row["custom_id"],
                    "result": {"type": "errored", "error": {"type": "fixture_error"}}
                }))
                .expect("serialize provider error"),
                vec![b'\n'],
            ]
            .concat(),
            "provider_error_results",
        ),
    ];
    for (case, bytes, reason) in cases {
        let mut download = base_download.clone();
        let mut execution = base_execution.clone();
        bind_mutated_download(
            &execution_dir,
            &mut download,
            &mut execution,
            artifact_path,
            &bytes,
        );
        let receipt = run_rejoin(&execution_dir, &manifest_path, case);
        assert_eq!(receipt["consumable"], false, "{case}");
        assert!(
            receipt["quarantine"]["reasons"]
                .as_array()
                .is_some_and(|reasons| reasons.iter().any(|value| value == reason)),
            "{case}: {}",
            receipt["quarantine"]
        );
    }

    let mut partial_download = base_download.clone();
    partial_download["jobs"][0]["status"] = Value::String("ready".to_string());
    let mut partial_execution = base_execution;
    bind_mutated_download(
        &execution_dir,
        &mut partial_download,
        &mut partial_execution,
        artifact_path,
        &base_artifact,
    );
    let partial = run_rejoin(&execution_dir, &manifest_path, "partial");
    assert_eq!(
        partial["quarantine"]["reasons"],
        serde_json::json!(["partial_jobs"])
    );

    fs::write(artifact_path, b"changed without receipt update\n").expect("change raw artifact");
    let changed = run_rejoin(&execution_dir, &manifest_path, "changed-artifact");
    assert!(changed["quarantine"]["reasons"]
        .as_array()
        .is_some_and(|reasons| reasons
            .iter()
            .any(|value| value == "lineage_or_artifact_errors")));

    fs::write(artifact_path, &base_artifact).expect("restore result artifact");
    let wrong_manifest_path = execution_dir.join("wrong-manifest.json");
    let mut wrong_manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    wrong_manifest["groups"][0]["requests"][0]["custom_id"] =
        Value::String("wrong-manifest-id".to_string());
    write_json(&wrong_manifest_path, &wrong_manifest);
    let wrong = run_rejoin(&execution_dir, &wrong_manifest_path, "wrong-manifest");
    assert!(wrong["quarantine"]["reasons"]
        .as_array()
        .is_some_and(|reasons| reasons
            .iter()
            .any(|value| value == "lineage_or_artifact_errors")));

    let execution_path = execution_dir.join("execution.json");
    let mut wrong_execution: Value =
        serde_json::from_slice(&fs::read(&execution_path).expect("read execution"))
            .expect("parse execution");
    wrong_execution["executionId"] = Value::String("caller-selected-execution".to_string());
    write_json(&execution_path, &wrong_execution);
    let wrong = run_rejoin(&execution_dir, &manifest_path, "wrong-execution");
    assert!(wrong["errors"].as_array().is_some_and(|errors| errors
        .iter()
        .any(|value| value == "wrong_execution_identity")));
    assert!(wrong["quarantine"]["reasons"]
        .as_array()
        .is_some_and(|reasons| reasons
            .iter()
            .any(|value| value == "lineage_or_artifact_errors")));
}

#[test]
fn durable_execution_rejects_tamper_before_advancing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);
    fs::write(execution.join("manifest.json"), "{}\n").expect("tamper manifest");

    let output = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_ne!(output.exit_code, 0);
    let failure = parse_json(&output.stdout, "tamper failure");
    assert_eq!(failure["ok"], false);
    assert!(failure["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("artifact changed")));
    assert!(!execution.join("submission.json").exists());
}

#[test]
fn durable_execution_rejects_every_lineage_and_transition_escape_hatch() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let prepared_execution = tmp.path().join("prepared-execution");
    let prepared = initialize(
        &tmp.path().join("prepared-requests.jsonl"),
        &prepared_execution,
    );
    let prepared_path = success_data(&prepared)["artifacts"]
        .as_array()
        .expect("prepared artifacts")
        .iter()
        .find(|artifact| artifact["role"] == "prepared_request")
        .and_then(|artifact| artifact["path"].as_str())
        .expect("prepared request path");
    fs::write(prepared_path, b"tampered prepared request\n").expect("tamper prepared request");
    let prepared_failure = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            prepared_execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_ne!(prepared_failure.exit_code, 0);
    assert!(!prepared_execution.join("submission.json").exists());

    let receipt_execution = tmp.path().join("receipt-execution");
    initialize(
        &tmp.path().join("receipt-requests.jsonl"),
        &receipt_execution,
    );
    advance(&receipt_execution);
    fs::write(
        receipt_execution.join("submission.json"),
        b"{\"kind\":\"harn.model_batch_submission_receipt\",\"jobs\":[]}\n",
    )
    .expect("tamper intermediate receipt");
    let receipt_failure = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            receipt_execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_ne!(receipt_failure.exit_code, 0);
    assert!(!receipt_execution.join("status.json").exists());

    let identity_execution = tmp.path().join("identity-execution");
    initialize(
        &tmp.path().join("identity-requests.jsonl"),
        &identity_execution,
    );
    let identity_path = identity_execution.join("execution.json");
    let mut identity: Value =
        serde_json::from_slice(&fs::read(&identity_path).expect("read execution"))
            .expect("parse execution");
    identity["jobIds"][0] = Value::String("caller-edited-job".to_string());
    write_json(&identity_path, &identity);
    let identity_failure = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            identity_execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_ne!(identity_failure.exit_code, 0);
    assert!(!identity_execution.join("submission.json").exists());

    let transition_execution = tmp.path().join("transition-execution");
    initialize(
        &tmp.path().join("transition-requests.jsonl"),
        &transition_execution,
    );
    let transition_path = transition_execution.join("execution.json");
    let mut transition: Value =
        serde_json::from_slice(&fs::read(&transition_path).expect("read execution"))
            .expect("parse execution");
    transition["phase"] = Value::String("caller_selected_phase".to_string());
    write_json(&transition_path, &transition);
    let transition_failure = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            transition_execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_ne!(transition_failure.exit_code, 0);
    let transition_error = parse_json(&transition_failure.stdout, "illegal transition");
    assert!(transition_error["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("cannot advance from phase")));
    assert!(!transition_execution.join("submission.json").exists());
}

#[test]
fn accepted_without_receipt_requires_reconciliation_and_never_retries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);

    let first = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[("HARN_MODELS_BATCH_TEST_KILL_POINT", "accepted_middle")],
    );
    assert_eq!(first.exit_code, 86);
    assert!(!execution.join("submission.json").exists());

    let before = fs::read(execution.join("execution.json")).expect("read execution before retry");
    let retry = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_ne!(retry.exit_code, 0);
    let failure = parse_json(&retry.stdout, "reconciliation failure");
    assert!(failure["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("requires reconciliation")));
    assert_eq!(
        before,
        fs::read(execution.join("execution.json")).expect("read execution after retry")
    );
    assert!(!execution.join("submission.json").exists());
}

#[test]
fn accepted_without_receipt_retries_the_same_deterministic_operation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);

    let prepare_path = execution.join("prepared/receipt.json");
    let mut prepare: Value =
        serde_json::from_slice(&fs::read(&prepare_path).expect("read prepare receipt"))
            .expect("parse prepare receipt");
    for job in prepare["jobs"].as_array_mut().expect("prepare jobs") {
        job["create_recovery"] = serde_json::json!({"mode": "deterministic_token"});
    }
    write_json(&prepare_path, &prepare);
    let prepare_sha = sha256(&fs::read(&prepare_path).expect("read mutated prepare receipt"));
    let execution_path = execution.join("execution.json");
    let mut execution_state: Value =
        serde_json::from_slice(&fs::read(&execution_path).expect("read execution state"))
            .expect("parse execution state");
    for artifact in execution_state["artifacts"]
        .as_array_mut()
        .expect("execution artifacts")
    {
        if artifact["role"] == "prepare_receipt" {
            artifact["sha256"] = Value::String(prepare_sha.clone());
        }
    }
    write_json(&execution_path, &execution_state);

    let first = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[("HARN_MODELS_BATCH_TEST_KILL_POINT", "accepted_middle")],
    );
    assert_eq!(first.exit_code, 86);
    let dispatching: Value =
        serde_json::from_slice(&fs::read(execution.join("execution.json")).expect("read state"))
            .expect("parse state");
    let operation_id = dispatching["operation"]["id"]
        .as_str()
        .expect("operation id")
        .to_string();
    assert_eq!(
        dispatching["operation"]["idempotencyMode"],
        "deterministic_client_token"
    );

    let resumed = advance(&execution);
    let state = success_data(&resumed);
    assert_eq!(state["phase"], "submitted");
    assert_eq!(state["operation"]["id"], operation_id);
    assert!(state["history"].as_array().is_some_and(|entries| entries
        .iter()
        .any(|entry| entry["detail"]["operation"] == "retry_planned"
            && entry["detail"]["operationId"] == operation_id)));
}

#[test]
fn pre_call_kill_resumes_the_planned_operation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);

    let killed = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[("HARN_MODELS_BATCH_TEST_KILL_POINT", "pre_call")],
    );
    assert_eq!(killed.exit_code, 86);
    assert!(!execution.join("submission.json").exists());
    let planned: Value =
        serde_json::from_slice(&fs::read(execution.join("execution.json")).expect("read planned"))
            .expect("parse planned");
    assert_eq!(planned["operation"]["status"], "planned");

    let resumed = advance(&execution);
    assert_eq!(success_data(&resumed)["phase"], "submitted");
}

#[test]
fn post_receipt_kill_keeps_the_committed_transition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);

    let killed = run(
        &[
            "models",
            "batch",
            "execute",
            "advance",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[("HARN_MODELS_BATCH_TEST_KILL_POINT", "post_receipt")],
    );
    assert_eq!(killed.exit_code, 86);
    let committed: Value = serde_json::from_slice(
        &fs::read(execution.join("execution.json")).expect("read committed"),
    )
    .expect("parse committed");
    assert_eq!(committed["phase"], "submitted");
    assert_eq!(committed["operation"]["status"], "committed");
    assert!(execution.join("submission.json").exists());

    let next = advance(&execution);
    assert_eq!(success_data(&next)["phase"], "completed");
    let history = success_data(&next)["history"].as_array().expect("history");
    assert_eq!(
        history
            .iter()
            .filter(|entry| {
                entry["to"] == "submitted" && entry["detail"]["operation"] == "committed"
            })
            .count(),
        1
    );
}

#[test]
fn durable_cancel_uses_the_same_committed_history() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);
    advance(&execution);

    let output = run(
        &[
            "models",
            "batch",
            "execute",
            "cancel",
            "--execution-dir",
            execution.to_str().expect("utf8 execution"),
            "--json",
        ],
        &[],
    );
    assert_eq!(
        output.exit_code, 0,
        "cancel stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    let state = parse_json(&output.stdout, "durable cancel");
    assert_eq!(success_data(&state)["phase"], "cancelled");
    let receipt: Value = serde_json::from_slice(
        &fs::read(execution.join("cancel.json")).expect("read cancel receipt"),
    )
    .expect("parse cancel receipt");
    assert_eq!(receipt["kind"], "harn.model_batch_cancel_receipt");
}

#[test]
fn concurrent_advance_commits_only_one_transition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let execution = tmp.path().join("execution");
    initialize(&requests, &execution);

    let args = [
        "models",
        "batch",
        "execute",
        "advance",
        "--execution-dir",
        execution.to_str().expect("utf8 execution"),
        "--json",
    ];
    let mut first = Command::new(harn_e2e_binary())
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn first advance");
    let second = Command::new(harn_e2e_binary())
        .args(args)
        .output()
        .expect("spawn second advance");
    let first_status = first.wait().expect("wait first advance");
    assert!(
        first_status.success() ^ second.status.success(),
        "exactly one concurrent writer should succeed"
    );

    let state: Value = serde_json::from_slice(
        &fs::read(execution.join("execution.json")).expect("read execution"),
    )
    .expect("parse execution");
    assert_eq!(state["phase"], "submitted");
    assert_eq!(state["revision"], 4);
    assert_eq!(
        state["history"]
            .as_array()
            .expect("execution history")
            .len(),
        4
    );
}
