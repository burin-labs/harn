use std::fs;

use super::support::{parse_json, run, success_data};

#[test]
fn models_batch_prepare_xai_jsonl_and_dry_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "eval",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "xai-fixture",
      "provider": "xai",
      "model": "grok-4",
      "workload": "eval",
      "endpoint": "provider_default",
      "tool_format": "native",
      "batch": {"api": true, "wire_format": "xai", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "xai_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "grade this"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "xai batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "xai");
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");
    assert_eq!(job["submit"]["upload"]["file"], job["request_file"]);
    assert_eq!(job["submit"]["upload"]["purpose"], serde_json::Value::Null);
    assert_eq!(
        job["submit"]["create_batch"]["input_file_id"],
        "<uploaded-file-id>"
    );

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("xai line"),
        "xai batch line",
    );
    assert_eq!(request["custom_id"], "xai_1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "grok-4");
    assert_eq!(request["body"]["messages"][0]["content"], "grade this");

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
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "xai batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "XAI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.x.ai/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);
    let status_value = parse_json(&status.stdout, "xai batch status");
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["ready_count"], 1);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "xai status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_xai".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["results_url"] = serde_json::Value::String(
            "https://api.x.ai/v1/batches/batch_xai/results?limit=100".to_string(),
        );
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize xai status"),
    )
    .expect("write xai status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "xai batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "results");
    assert_eq!(artifacts[0]["handle"], "batch_xai");
    assert_eq!(artifacts[0]["operation"]["credential_env"], "XAI_API_KEY");
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.x.ai/v1/batches/batch_xai/results"
    );
}

#[test]
fn models_batch_prepare_anthropic_inline_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "judge",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "anthropic-fixture",
      "provider": "anthropic",
      "model": "claude-haiku-4-5-20251001",
      "workload": "judge",
      "endpoint": "provider_default",
      "tool_format": "native",
      "batch": {"api": true, "wire_format": "anthropic_messages", "input_mode": "inline_requests"},
      "requests": [
        {
          "custom_id": "anth_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"max_tokens": 32, "messages": [{"role": "user", "content": "label this"}]}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "anthropic");
    assert_eq!(job["request_format"], "json_requests");
    assert_eq!(job["endpoint"], "/v1/messages/batches");
    assert_eq!(job["submit"]["operation"], "POST /v1/messages/batches");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_value = parse_json(
        &fs::read_to_string(request_file).expect("read request file"),
        "anthropic request body",
    );
    assert_eq!(request_value["requests"][0]["custom_id"], "anth_1");
    assert_eq!(
        request_value["requests"][0]["params"]["model"],
        "claude-haiku-4-5-20251001"
    );
    assert_eq!(
        request_value["requests"][0]["params"]["messages"][0]["content"],
        "label this"
    );
}

#[test]
fn models_batch_prepare_gemini_and_mistral_request_shapes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "corpus",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 2},
  "requestCount": 2,
  "groupCount": 2,
  "groups": [
    {
      "id": "gemini-fixture",
      "provider": "gemini",
      "model": "gemini-2.5-flash-lite",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "gemini", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "gemini_1",
          "source_line": 1,
          "source_sha256": "fixture-a",
          "metadata": {},
          "request": {"contents": [{"role": "user", "parts": [{"text": "refresh"}]}]}
        }
      ]
    },
    {
      "id": "mistral-fixture",
      "provider": "mistral",
      "model": "mistral-small-2603",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "mistral", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "mistral_1",
          "source_line": 2,
          "source_sha256": "fixture-b",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "refresh"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "batch prepare");
    let report = success_data(&prepared_value);
    let jobs = report["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 2);
    let gemini = jobs
        .iter()
        .find(|job| job["provider"] == "gemini")
        .expect("gemini job");
    let mistral = jobs
        .iter()
        .find(|job| job["provider"] == "mistral")
        .expect("mistral job");

    assert_eq!(gemini["endpoint"], "batchGenerateContent");
    assert_eq!(gemini["submit"]["operation"], "batches.create");
    let gemini_line = fs::read_to_string(gemini["request_file"].as_str().expect("gemini file"))
        .expect("read gemini file");
    let gemini_request = parse_json(
        gemini_line.lines().next().expect("gemini line"),
        "gemini line",
    );
    assert_eq!(gemini_request["key"], "gemini_1");
    assert_eq!(
        gemini_request["request"]["contents"][0]["parts"][0]["text"],
        "refresh"
    );
    assert!(
        gemini_request["request"]["model"].is_null(),
        "Gemini batch rows should keep model at job creation"
    );

    assert_eq!(mistral["endpoint"], "/v1/chat/completions");
    assert_eq!(mistral["submit"]["operation"], "POST /v1/batch/jobs");
    let mistral_line = fs::read_to_string(mistral["request_file"].as_str().expect("mistral file"))
        .expect("read mistral file");
    let mistral_request = parse_json(
        mistral_line.lines().next().expect("mistral line"),
        "mistral line",
    );
    assert_eq!(mistral_request["custom_id"], "mistral_1");
    assert_eq!(mistral_request["body"]["model"], "mistral-small-2603");
}

#[test]
fn models_batch_prepare_gemini_and_dry_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "corpus",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "gemini-fixture",
      "provider": "gemini",
      "model": "gemini-2.5-flash-lite",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "gemini", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "gemini_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"contents": [{"role": "user", "parts": [{"text": "refresh"}]}]}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "gemini batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "gemini");
    assert_eq!(job["endpoint"], "batchGenerateContent");
    assert_eq!(job["submit"]["operation"], "batches.create");
    assert_eq!(job["submit"]["input"]["mode"], "file_api_jsonl");
    assert_eq!(job["submit"]["input"]["file"], job["request_file"]);

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("gemini line"),
        "gemini batch line",
    );
    assert_eq!(request["key"], "gemini_1");
    assert_eq!(
        request["request"]["contents"][0]["parts"][0]["text"],
        "refresh"
    );
    assert!(
        request["request"]["model"].is_null(),
        "Gemini batch rows should keep model at job creation"
    );

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
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "gemini batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "GEMINI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://generativelanguage.googleapis.com"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);
    let status_value = parse_json(&status.stdout, "gemini batch status");
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["ready_count"], 1);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "gemini status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] =
            serde_json::Value::String("batches/gemini-batch".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("JOB_STATE_SUCCEEDED".to_string());
        jobs[0]["responses_file"] = serde_json::Value::String("files/gemini-output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize gemini status"),
    )
    .expect("write gemini status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "gemini batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "responses");
    assert_eq!(artifacts[0]["handle"], "files/gemini-output");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "GEMINI_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://generativelanguage.googleapis.com/download/v1beta/files/gemini-output:download"
    );
}

#[test]
fn models_batch_prepare_parasail_openai_compatible_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "eval",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "parasail-fixture",
      "provider": "parasail",
      "model": "openai/gpt-oss-120b",
      "workload": "eval",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "openai", "input_mode": "jsonl_file"},
      "requests": [
        {
          "custom_id": "parasail_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "grade this"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "parasail batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "parasail");
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");
    assert_eq!(job["submit"]["upload"]["purpose"], "batch");
    assert_eq!(job["submit"]["create_batch"]["completion_window"], "24h");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("parasail line"),
        "parasail batch line",
    );
    assert_eq!(request["custom_id"], "parasail_1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "openai/gpt-oss-120b");
    assert_eq!(request["body"]["messages"][0]["content"], "grade this");

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
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "parasail batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], "parasail");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "PARASAIL_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.saas.parasail.io/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "parasail status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_parasail".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_parasail_output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize parasail status"),
    )
    .expect("write parasail status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "parasail batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_parasail_output");
    assert_eq!(artifacts[0]["operation"]["provider"], "parasail");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "PARASAIL_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.saas.parasail.io/v1/files/file_parasail_output/content"
    );
}

#[test]
fn models_batch_prepare_fireworks_and_dry_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "corpus",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "fireworks-fixture",
      "provider": "fireworks",
      "model": "accounts/fireworks/models/gpt-oss-120b",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "fireworks", "input_mode": "jsonl_file"},
      "requests": [
        {
          "custom_id": "fireworks_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "refresh"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "fireworks batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "fireworks");
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

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("fireworks line"),
        "fireworks batch line",
    );
    assert_eq!(request["custom_id"], "fireworks_1");
    assert_eq!(request["body"]["messages"][0]["content"], "refresh");
    assert_eq!(request["body"]["max_tokens"], 16);
    assert!(
        request["body"]["model"].is_null(),
        "Fireworks batch rows keep model at job creation"
    );
    assert!(
        request["method"].is_null(),
        "Fireworks rows omit OpenAI method"
    );
    assert!(request["url"].is_null(), "Fireworks rows omit OpenAI url");

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
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "fireworks batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "FIREWORKS_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.fireworks.ai/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "fireworks status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("fw-batch".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("COMPLETED".to_string());
        jobs[0]["output_dataset_id"] = serde_json::Value::String("fw-output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize fireworks status"),
    )
    .expect("write fireworks status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[("HARN_BATCH_FIREWORKS_ACCOUNT_ID", "acct-test")],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "fireworks batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "download-endpoint");
    assert_eq!(artifacts[0]["handle"], "fw-output");
    assert_eq!(artifacts[0]["operation"]["provider"], "fireworks");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "FIREWORKS_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.fireworks.ai/v1/accounts/acct-test/datasets/fw-output:getDownloadEndpoint"
    );
}
