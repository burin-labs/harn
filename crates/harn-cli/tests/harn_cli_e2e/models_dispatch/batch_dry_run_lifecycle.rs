//! Table-driven prepare -> submit -> status -> download dry-run lifecycle for
//! every provider whose batch adapter follows the file-based shape. Each case
//! carries only what is provider-specific; the shared flow lives in
//! `run_dry_run_lifecycle`.

use std::fs;

use rstest::rstest;
use serde_json::{json, Value};

use super::support::{batch_manifest, parse_json, prepare_batch, run, success_data};

enum Input {
    /// A hand-written manifest passed straight to `prepare`.
    Manifest(Value),
    /// A requests JSONL that `batch manifest` turns into the manifest.
    Requests {
        model: &'static str,
        jsonl: &'static str,
    },
}

struct DryRunCase {
    provider: &'static str,
    input: Input,
    /// Provider-specific assertions on the prepared job report.
    check_prepared: fn(&Value),
    /// Provider-specific assertions on the parsed rows of the request file.
    check_request_rows: fn(&[Value]),
    credential_env: &'static str,
    base_url: &'static str,
    /// Fields written onto the status receipt's job to mark it completed.
    completed_fields: &'static [(&'static str, &'static str)],
    download_env: &'static [(&'static str, &'static str)],
    artifact_label: &'static str,
    artifact_handle: &'static str,
    artifact_operation: &'static str,
}

fn chat_request(content: &str) -> Value {
    json!({"messages": [{"role": "user", "content": content}], "max_tokens": 16})
}

/// Asserts the OpenAI-style request row shared by the chat-completions providers.
fn assert_openai_row(row: &Value, custom_id: &str, model: &str) {
    assert_eq!(row["custom_id"], custom_id);
    assert_eq!(row["method"], "POST");
    assert_eq!(row["url"], "/v1/chat/completions");
    assert_eq!(row["body"]["model"], model);
}

fn xai_case() -> DryRunCase {
    DryRunCase {
        provider: "xai",
        input: Input::Manifest(batch_manifest(
            "xai",
            "grok-4",
            "eval",
            "native",
            "xai",
            "jsonl_or_inline",
            chat_request("grade this"),
        )),
        check_prepared: |job| {
            assert_eq!(job["endpoint"], "/v1/chat/completions");
            assert_eq!(job["submit"]["operation"], "POST /v1/batches");
            assert_eq!(job["submit"]["upload"]["file"], job["request_file"]);
            assert_eq!(job["submit"]["upload"]["purpose"], Value::Null);
            assert_eq!(
                job["submit"]["create_batch"]["input_file_id"],
                "<uploaded-file-id>"
            );
        },
        check_request_rows: |rows| {
            assert_openai_row(&rows[0], "xai_1", "grok-4");
            assert_eq!(rows[0]["body"]["messages"][0]["content"], "grade this");
        },
        credential_env: "XAI_API_KEY",
        base_url: "https://api.x.ai/v1",
        completed_fields: &[
            ("provider_batch_id", "batch_xai"),
            ("provider_status", "completed"),
            (
                "results_url",
                "https://api.x.ai/v1/batches/batch_xai/results?limit=100",
            ),
        ],
        download_env: &[],
        artifact_label: "results",
        artifact_handle: "batch_xai",
        artifact_operation: "GET https://api.x.ai/v1/batches/batch_xai/results",
    }
}

fn gemini_case() -> DryRunCase {
    DryRunCase {
        provider: "gemini",
        input: Input::Manifest(batch_manifest(
            "gemini",
            "gemini-2.5-flash-lite",
            "corpus",
            "json",
            "gemini",
            "jsonl_or_inline",
            json!({"contents": [{"role": "user", "parts": [{"text": "refresh"}]}]}),
        )),
        check_prepared: |job| {
            assert_eq!(job["endpoint"], "batchGenerateContent");
            assert_eq!(job["submit"]["operation"], "batches.create");
            assert_eq!(job["submit"]["input"]["mode"], "file_api_jsonl");
            assert_eq!(job["submit"]["input"]["file"], job["request_file"]);
        },
        check_request_rows: |rows| {
            assert_eq!(rows[0]["key"], "gemini_1");
            assert_eq!(
                rows[0]["request"]["contents"][0]["parts"][0]["text"],
                "refresh"
            );
            assert!(
                rows[0]["request"]["model"].is_null(),
                "Gemini batch rows should keep model at job creation"
            );
        },
        credential_env: "GEMINI_API_KEY",
        base_url: "https://generativelanguage.googleapis.com",
        completed_fields: &[
            ("provider_batch_id", "batches/gemini-batch"),
            ("provider_status", "JOB_STATE_SUCCEEDED"),
            ("responses_file", "files/gemini-output"),
        ],
        download_env: &[],
        artifact_label: "responses",
        artifact_handle: "files/gemini-output",
        artifact_operation: "GET https://generativelanguage.googleapis.com/download/v1beta/files/gemini-output:download",
    }
}

fn parasail_case() -> DryRunCase {
    DryRunCase {
        provider: "parasail",
        input: Input::Manifest(batch_manifest(
            "parasail",
            "openai/gpt-oss-120b",
            "eval",
            "json",
            "openai",
            "jsonl_file",
            chat_request("grade this"),
        )),
        check_prepared: |job| {
            assert_eq!(job["endpoint"], "/v1/chat/completions");
            assert_eq!(job["batch"]["wire_format"], "openai");
            assert_eq!(job["submit"]["operation"], "POST /v1/batches");
            assert_eq!(job["submit"]["upload"]["purpose"], "batch");
            assert_eq!(job["submit"]["create_batch"]["completion_window"], "24h");
        },
        check_request_rows: |rows| {
            assert_openai_row(&rows[0], "parasail_1", "openai/gpt-oss-120b");
            assert_eq!(rows[0]["body"]["messages"][0]["content"], "grade this");
        },
        credential_env: "PARASAIL_API_KEY",
        base_url: "https://api.saas.parasail.io/v1",
        completed_fields: &[
            ("provider_batch_id", "batch_parasail"),
            ("provider_status", "completed"),
            ("output_file_id", "file_parasail_output"),
        ],
        download_env: &[],
        artifact_label: "output",
        artifact_handle: "file_parasail_output",
        artifact_operation:
            "GET https://api.saas.parasail.io/v1/files/file_parasail_output/content",
    }
}

fn fireworks_case() -> DryRunCase {
    DryRunCase {
        provider: "fireworks",
        input: Input::Manifest(batch_manifest(
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            "corpus",
            "json",
            "fireworks",
            "jsonl_file",
            chat_request("refresh"),
        )),
        check_prepared: |job| {
            assert_eq!(job["endpoint"], "/v1/chat/completions");
            assert_eq!(job["batch"]["wire_format"], "fireworks");
            assert_eq!(
                job["submit"]["operation"],
                "POST /v1/accounts/{account_id}/batchInferenceJobs"
            );
            assert_eq!(
                job["submit"]["upload"]["upload_dataset"],
                "POST /v1/accounts/{account_id}/datasets/{dataset_id}:upload"
            );
            assert_eq!(job["submit"]["request_line_shape"], "{custom_id, body}");
        },
        check_request_rows: |rows| {
            let row = &rows[0];
            assert_eq!(row["custom_id"], "fireworks_1");
            assert_eq!(row["body"]["messages"][0]["content"], "refresh");
            assert_eq!(row["body"]["max_tokens"], 16);
            assert!(
                row["body"]["model"].is_null(),
                "Fireworks batch rows keep model at job creation"
            );
            assert!(row["method"].is_null(), "Fireworks rows omit OpenAI method");
            assert!(row["url"].is_null(), "Fireworks rows omit OpenAI url");
        },
        credential_env: "FIREWORKS_API_KEY",
        base_url: "https://api.fireworks.ai/v1",
        completed_fields: &[
            ("provider_batch_id", "fw-batch"),
            ("provider_status", "COMPLETED"),
            ("output_dataset_id", "fw-output"),
        ],
        download_env: &[("HARN_BATCH_FIREWORKS_ACCOUNT_ID", "acct-test")],
        artifact_label: "download-endpoint",
        artifact_handle: "fw-output",
        artifact_operation: "GET https://api.fireworks.ai/v1/accounts/acct-test/datasets/fw-output:getDownloadEndpoint",
    }
}

fn together_case() -> DryRunCase {
    DryRunCase {
        provider: "together",
        input: Input::Requests {
            model: "together/meta-models/Muse-Glimmer-30B",
            jsonl: r#"{"custom_id":"together-case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
{"custom_id":"together-case-2","body":{"model":"together/meta-models/Muse-Glimmer-30B","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}}
{"custom_id":"together-case-3","body":{"model":"meta-models/Muse-Glimmer-30B","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}}
"#,
        },
        check_prepared: |job| {
            assert_eq!(job["model"], "together/meta-models/Muse-Glimmer-30B");
            assert_eq!(job["batch"]["wire_format"], "openai");
            assert_eq!(job["batch"]["discount_percent"], 50);
            assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
            assert_eq!(job["endpoint"], "/v1/chat/completions");
            assert_eq!(job["submit"]["operation"], "POST /v1/batches");
            assert_eq!(job["submit"]["upload"]["purpose"], "batch-api");
            assert_eq!(
                job["submit"]["create_batch"]["input_file_id"],
                "<uploaded-file-id>"
            );
        },
        check_request_rows: |rows| {
            assert_openai_row(&rows[0], "together-case-1", "meta-models/Muse-Glimmer-30B");
            assert_eq!(rows.len(), 3);
            for row in rows {
                assert_eq!(row["body"]["model"], "meta-models/Muse-Glimmer-30B");
            }
        },
        credential_env: "TOGETHER_AI_API_KEY",
        base_url: "https://api.together.xyz/v1",
        completed_fields: &[
            ("provider_batch_id", "batch_together"),
            ("provider_status", "completed"),
            ("output_file_id", "file_together_output"),
        ],
        download_env: &[],
        artifact_label: "output",
        artifact_handle: "file_together_output",
        artifact_operation: "GET https://api.together.xyz/v1/files/file_together_output/content",
    }
}

fn groq_case() -> DryRunCase {
    DryRunCase {
        provider: "groq",
        input: Input::Requests {
            model: "qwen/qwen3.8-27b",
            jsonl: r#"{"custom_id":"groq-case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
"#,
        },
        check_prepared: |job| {
            assert_eq!(job["batch"]["wire_format"], "openai");
            assert_eq!(job["batch"]["discount_percent"], 50);
            assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
            assert_eq!(job["endpoint"], "/v1/chat/completions");
            assert_eq!(job["submit"]["operation"], "POST /v1/batches");
        },
        check_request_rows: |rows| {
            assert_openai_row(&rows[0], "groq-case-1", "qwen/qwen3.8-27b");
        },
        credential_env: "GROQ_API_KEY",
        base_url: "https://api.groq.com/openai/v1",
        completed_fields: &[
            ("provider_batch_id", "batch_groq"),
            ("provider_status", "completed"),
            ("output_file_id", "file_groq_output"),
        ],
        download_env: &[],
        artifact_label: "output",
        artifact_handle: "file_groq_output",
        artifact_operation: "GET https://api.groq.com/openai/v1/files/file_groq_output/content",
    }
}

fn path_str(path: &std::path::Path) -> &str {
    path.to_str().expect("utf8 path")
}

fn first_job<'a>(value: &'a Value, what: &str) -> &'a Value {
    &value["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("{what} jobs"))[0]
}

#[rstest]
#[case::xai(xai_case())]
#[case::gemini(gemini_case())]
#[case::parasail(parasail_case())]
#[case::fireworks(fireworks_case())]
#[case::together(together_case())]
#[case::groq(groq_case())]
fn models_batch_prepare_and_dry_run_lifecycle(#[case] case: DryRunCase) {
    let provider = case.provider;
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    match &case.input {
        Input::Manifest(value) => fs::write(
            &manifest_path,
            serde_json::to_string_pretty(value).expect("serialize manifest"),
        )
        .expect("write manifest"),
        Input::Requests { model, jsonl } => {
            let requests_path = tmp.path().join("requests.jsonl");
            fs::write(&requests_path, jsonl).expect("write requests");
            let manifest = run(
                &[
                    "models",
                    "batch",
                    "manifest",
                    "--provider",
                    provider,
                    "--model",
                    model,
                    "--requests",
                    path_str(&requests_path),
                    "--out",
                    path_str(&manifest_path),
                    "--json",
                ],
                &[],
            );
            assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);
        }
    }

    let prepared_value = prepare_batch(
        &manifest_path,
        &out_dir,
        &format!("{provider} batch prepare"),
    );
    let report = success_data(&prepared_value);
    let job = first_job(report, "prepared");
    assert_eq!(job["provider"], provider);
    (case.check_prepared)(job);

    let request_text = fs::read_to_string(job["request_file"].as_str().expect("request_file"))
        .expect("read request file");
    let rows: Vec<Value> = request_text
        .lines()
        .map(|line| parse_json(line, &format!("{provider} batch line")))
        .collect();
    (case.check_request_rows)(&rows);

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            path_str(&submission_path),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, &format!("{provider} batch submit"));
    let submitted_job = first_job(success_data(&submitted_value), "submitted");
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], provider);
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        case.credential_env
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        case.base_url
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            path_str(&submission_path),
            "--out",
            path_str(&status_path),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);
    let status_value = parse_json(&status.stdout, &format!("{provider} batch status"));
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["ready_count"], 1);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        &format!("{provider} status receipt"),
    );
    status_receipt["status"] = json!("completed");
    status_receipt["completedCount"] = json!(1);
    status_receipt["readyCount"] = json!(0);
    let status_job = &mut status_receipt["jobs"]
        .as_array_mut()
        .expect("mutable status jobs")[0];
    status_job["status"] = json!("completed");
    for (key, value) in case.completed_fields {
        status_job[*key] = json!(value);
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize status"),
    )
    .expect("write status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            path_str(&status_path),
            "--out-dir",
            path_str(&results_dir),
            "--dry-run",
            "--json",
        ],
        case.download_env,
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, &format!("{provider} batch download"));
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 1);
    let artifact = &first_job(download_report, "download")["artifacts"]
        .as_array()
        .expect("download artifacts")[0];
    assert_eq!(artifact["label"], case.artifact_label);
    assert_eq!(artifact["handle"], case.artifact_handle);
    assert_eq!(artifact["operation"]["provider"], provider);
    assert_eq!(artifact["operation"]["credential_env"], case.credential_env);
    assert_eq!(artifact["operation"]["operation"], case.artifact_operation);
}
