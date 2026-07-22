use std::fs;

use super::support::{parse_json, run, success_data};

// - models batch ----------------------------------------------------------

#[test]
fn models_batch_plan_reports_harn_live_adapter_support() {
    let harn = run(&["models", "batch", "plan", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "batch plan");
    let report = success_data(&harn_value);
    let models = report["models"].as_array().expect("models");
    let openai = models
        .iter()
        .find(|model| model["provider"] == "openai")
        .expect("openai batch model");
    assert_eq!(openai["batch"]["wire_format"], "openai");
    assert_eq!(openai["batch"]["max_requests"], 50_000);
    assert_eq!(openai["batch"]["max_input_bytes"], 209_715_200);
    assert_eq!(openai["batch"]["result_retention_days"], 30);
    assert_eq!(openai["batch"]["result_ordering"], "custom_id_rejoin");
    assert_eq!(openai["batch"]["partial_failure"], "per_request");
    assert_eq!(openai["batch"]["cancellation"], "supported");
    assert!(
        openai["batch"]["security_notes"]
            .as_array()
            .is_some_and(|notes| !notes.is_empty()),
        "OpenAI batch plan should include public storage/security notes"
    );
    assert!(
        openai["batch"]["operational_notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note.as_str().unwrap_or("").contains("one model"))),
        "OpenAI batch plan should include provider grouping constraints"
    );
    assert_eq!(openai["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(openai["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(openai["batch"]["harn_live_adapter"]["cancel"], true);
    assert_eq!(openai["batch"]["harn_live_adapter"]["download"], true);

    let xai = models
        .iter()
        .find(|model| model["provider"] == "xai")
        .expect("xai batch model");
    assert_eq!(xai["batch"]["wire_format"], "xai");
    assert_eq!(xai["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(xai["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(xai["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(xai["batch"]["harn_live_adapter"]["download"], true);

    let groq = models
        .iter()
        .find(|model| model["provider"] == "groq")
        .expect("groq batch model");
    assert_eq!(groq["batch"]["wire_format"], "openai");
    assert_eq!(groq["batch"]["discount_percent"], 50);
    assert_eq!(groq["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(groq["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(groq["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(groq["batch"]["harn_live_adapter"]["download"], true);

    let together = models
        .iter()
        .find(|model| model["provider"] == "together")
        .expect("together batch model");
    assert_eq!(together["batch"]["wire_format"], "openai");
    assert_eq!(together["batch"]["discount_percent"], 50);
    assert_eq!(together["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(together["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(together["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(together["batch"]["harn_live_adapter"]["download"], true);

    let gemini = models
        .iter()
        .find(|model| model["provider"] == "gemini")
        .expect("gemini batch model");
    assert_eq!(gemini["batch"]["wire_format"], "gemini");
    assert_eq!(gemini["batch"]["max_input_bytes"], 2_147_483_648_u64);
    assert_eq!(gemini["batch"]["result_ordering"], "custom_id_rejoin");
    assert_eq!(gemini["batch"]["partial_failure"], "per_request");
    assert_eq!(gemini["batch"]["cancellation"], "supported");
    assert!(
        gemini["batch"]["operational_notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note.as_str().unwrap_or("").contains("not idempotent"))),
        "Gemini batch plan should surface create-retry idempotency risk"
    );
    assert_eq!(gemini["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(gemini["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(gemini["batch"]["harn_live_adapter"]["cancel"], true);
    assert_eq!(gemini["batch"]["harn_live_adapter"]["download"], true);

    let fireworks = models
        .iter()
        .find(|model| model["provider"] == "fireworks")
        .expect("fireworks batch model");
    assert_eq!(fireworks["batch"]["wire_format"], "fireworks");
    assert_eq!(fireworks["batch"]["discount_percent"], 50);
    assert!(
        fireworks["batch"]["operational_notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note.as_str().unwrap_or("").contains("model-specific"))),
        "Fireworks batch plan should surface model-specific capability constraints"
    );
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["download"], true);

    let human = run(&["models", "batch", "plan", "--provider", "gemini"], &[]);
    assert_eq!(human.exit_code, 0, "harn stderr={}", human.stderr);
    assert!(human.stdout.contains("live submit"), "{}", human.stdout);
}

#[test]
fn models_batch_manifest_and_dry_run_together_openai_compatible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"together-case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "together",
            "--model",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

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
    let prepared_value = parse_json(&prepared.stdout, "together batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "together");
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

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("together line"),
        "together batch line",
    );
    assert_eq!(request["custom_id"], "together-case-1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(
        request["body"]["model"],
        "meta-llama/Llama-3.3-70B-Instruct-Turbo"
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
    let submitted_value = parse_json(&submitted.stdout, "together batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], "together");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "TOGETHER_AI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.together.xyz/v1"
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
        "together status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_together".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_together_output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize together status"),
    )
    .expect("write together status receipt");

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
    let download_value = parse_json(&download.stdout, "together batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_together_output");
    assert_eq!(artifacts[0]["operation"]["provider"], "together");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "TOGETHER_AI_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.together.xyz/v1/files/file_together_output/content"
    );
}

#[test]
fn models_batch_manifest_and_dry_run_groq_openai_compatible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"groq-case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "groq",
            "--model",
            "llama-3.1-8b-instant",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

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
    let prepared_value = parse_json(&prepared.stdout, "groq batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "groq");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["batch"]["discount_percent"], 50);
    assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("groq line"),
        "groq batch line",
    );
    assert_eq!(request["custom_id"], "groq-case-1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "llama-3.1-8b-instant");

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
    let submitted_value = parse_json(&submitted.stdout, "groq batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], "groq");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "GROQ_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.groq.com/openai/v1"
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
        "groq status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_groq".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_groq_output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize groq status"),
    )
    .expect("write groq status receipt");

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
    let download_value = parse_json(&download.stdout, "groq batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_groq_output");
    assert_eq!(artifacts[0]["operation"]["provider"], "groq");
    assert_eq!(artifacts[0]["operation"]["credential_env"], "GROQ_API_KEY");
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.groq.com/openai/v1/files/file_groq_output/content"
    );
}

#[test]
fn models_batch_manifest_and_prepare_openai_responses_endpoint_override() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"responses-case-1","endpoint":"/v1/responses","input":"grade this","max_output_tokens":64}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

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
    let prepared_value = parse_json(&prepared.stdout, "responses batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "openai");
    assert_eq!(job["endpoint"], "/v1/responses");
    assert_eq!(job["submit"]["create_batch"]["endpoint"], "/v1/responses");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("responses line"),
        "responses batch line",
    );
    assert_eq!(request["custom_id"], "responses-case-1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/responses");
    assert_eq!(request["body"]["model"], "gpt-4o-mini");
    assert_eq!(request["body"]["input"], "grade this");
    assert_eq!(request["body"]["max_output_tokens"], 64);
}

#[test]
fn models_batch_manifest_rejects_streaming_request_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    fs::write(
        &requests_path,
        r#"{"custom_id":"stream-case-1","body":{"messages":[{"role":"user","content":"grade this"}]},"stream":true}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 1, "harn stdout={}", manifest.stdout);
    assert!(
        !manifest_path.exists(),
        "failed manifest should not be written"
    );
    let value = parse_json(&manifest.stdout, "streaming manifest failure");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "batch_manifest_failed");
    let errors = value["error"]["details"]["errors"]
        .as_array()
        .expect("failure errors")
        .iter()
        .map(|entry| entry.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(errors.contains("line 1"), "errors={errors}");
    assert!(
        errors.contains("custom_id=stream-case-1"),
        "errors={errors}"
    );
    assert!(errors.contains("stream: true"), "errors={errors}");
}

#[test]
fn models_batch_prepare_rejects_streaming_manifest_requests() {
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
      "id": "openai-streaming-fixture",
      "provider": "openai",
      "model": "gpt-4o-mini",
      "workload": "eval",
      "endpoint": "/v1/chat/completions",
      "tool_format": "native",
      "batch": {"api": true, "wire_format": "openai", "input_mode": "jsonl_file"},
      "requests": [
        {
          "custom_id": "stream-case-1",
          "source_line": 7,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {
            "messages": [{"role": "user", "content": "grade this"}],
            "stream": true
          }
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
    assert_eq!(prepared.exit_code, 1, "harn stdout={}", prepared.stdout);
    assert!(
        !out_dir.exists(),
        "prepare should reject before writing provider artifacts"
    );
    let value = parse_json(&prepared.stdout, "streaming prepare failure");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "batch_prepare_failed");
    let errors = value["error"]["details"]["errors"]
        .as_array()
        .expect("failure errors")
        .iter()
        .map(|entry| entry.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        errors.contains("group openai-streaming-fixture line 7"),
        "errors={errors}"
    );
    assert!(
        errors.contains("custom_id=stream-case-1"),
        "errors={errors}"
    );
    assert!(errors.contains("stream: true"), "errors={errors}");
}

#[test]
fn models_batch_manifest_and_prepare_openai_jsonl() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":64}
{"id":"case-2","body":{"messages":[{"role":"user","content":"grade that"}],"max_tokens":32}}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

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
    assert_eq!(report["job_count"], 1);
    assert_eq!(report["request_count"], 2);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "openai");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(job["lifecycle"]["phase"], "prepare");
    assert_eq!(job["lifecycle"]["state"], "prepared");
    assert_eq!(job["lifecycle"]["terminal"], false);
    assert_eq!(report["lifecycle"]["state"], "prepared");
    assert_eq!(report["lifecycle"]["counts"]["prepared"], 1);
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["request_format"], "jsonl");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let lines: Vec<&str> = request_text.lines().collect();
    assert_eq!(lines.len(), 2, "request_text={request_text}");
    let first = parse_json(lines[0], "first openai batch line");
    assert_eq!(first["custom_id"], "case-1");
    assert_eq!(first["method"], "POST");
    assert_eq!(first["url"], "/v1/chat/completions");
    assert_eq!(first["body"]["model"], "gpt-4o-mini");
    assert_eq!(first["body"]["messages"][0]["content"], "grade this");

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let receipt = parse_json(
        &fs::read_to_string(receipt_path).expect("read receipt"),
        "prepare receipt",
    );
    assert_eq!(receipt["kind"], "harn.model_batch_prepare_receipt");
    assert_eq!(receipt["status"], "prepared");
    assert_eq!(receipt["lifecycle"]["phase"], "prepare");
    assert_eq!(receipt["lifecycle"]["counts"]["prepared"], 1);
    assert_eq!(
        receipt["jobs"][0]["request_file_sha256"],
        job["request_file_sha256"]
    );

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
    let submitted_value = parse_json(&submitted.stdout, "batch submit");
    let submission = success_data(&submitted_value);
    assert_eq!(submission["dry_run"], true);
    assert_eq!(submission["job_count"], 1);
    assert_eq!(submission["ready_count"], 1);
    assert_eq!(submission["submitted_count"], 0);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["lifecycle"]["phase"], "submit");
    assert_eq!(submitted_job["lifecycle"]["state"], "ready");
    assert_eq!(submitted_job["lifecycle"]["dry_run"], true);
    assert_eq!(submitted_job["lifecycle"]["cancelable"], false);
    assert_eq!(submission["lifecycle"]["state"], "dry_run");
    assert_eq!(submission["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(submitted_job["provider"], "openai");
    assert_eq!(
        submitted_job["request_file_sha256"],
        job["request_file_sha256"]
    );
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["auth"],
        "OPENAI_API_KEY=<redacted>"
    );

    let submission_receipt = parse_json(
        &fs::read_to_string(&submission_path).expect("read submission receipt"),
        "submission receipt",
    );
    assert_eq!(
        submission_receipt["kind"],
        "harn.model_batch_submission_receipt"
    );
    assert_eq!(submission_receipt["status"], "dry_run");
    assert_eq!(submission_receipt["lifecycle"]["phase"], "submit");
    assert_eq!(submission_receipt["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(submission_receipt["jobs"][0]["status"], "ready");

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
    let status_value = parse_json(&status.stdout, "batch status");
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["status"], "dry_run");
    assert_eq!(status_report["job_count"], 1);
    assert_eq!(status_report["ready_count"], 1);
    assert_eq!(status_report["completed_count"], 0);
    assert_eq!(status_report["lifecycle"]["phase"], "status");
    assert_eq!(status_report["lifecycle"]["state"], "dry_run");
    assert_eq!(status_report["lifecycle"]["counts"]["ready"], 1);
    let status_job = &status_report["jobs"].as_array().expect("status jobs")[0];
    assert_eq!(status_job["status"], "ready");
    assert_eq!(status_job["lifecycle"]["state"], "ready");
    assert_eq!(status_job["status_checked"], false);
    assert_eq!(status_job["provider"], "openai");
    assert_eq!(
        status_job["provider_batch_id"],
        serde_json::Value::String(String::new())
    );

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "status receipt",
    );
    assert_eq!(status_receipt["kind"], "harn.model_batch_status_receipt");
    assert_eq!(status_receipt["status"], "dry_run");
    assert_eq!(status_receipt["lifecycle"]["phase"], "status");
    assert_eq!(status_receipt["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(status_receipt["jobs"][0]["status"], "ready");

    status_receipt["status"] = serde_json::Value::String("running".to_string());
    status_receipt["runningCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("running".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_test".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("in_progress".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize running status"),
    )
    .expect("write running status receipt");

    let cancel_path = tmp.path().join("cancel.json");
    let cancel = run(
        &[
            "models",
            "batch",
            "cancel",
            "--receipt",
            status_path.to_str().expect("utf8 status path"),
            "--out",
            cancel_path.to_str().expect("utf8 cancel path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(cancel.exit_code, 0, "harn stderr={}", cancel.stderr);
    let cancel_value = parse_json(&cancel.stdout, "batch cancel");
    let cancel_report = success_data(&cancel_value);
    assert_eq!(cancel_report["dry_run"], true);
    assert_eq!(cancel_report["status"], "dry_run");
    assert_eq!(cancel_report["job_count"], 1);
    assert_eq!(cancel_report["cancelable_count"], 1);
    assert_eq!(cancel_report["skipped_count"], 0);
    assert_eq!(cancel_report["lifecycle"]["phase"], "cancel");
    assert_eq!(cancel_report["lifecycle"]["state"], "dry_run");
    assert_eq!(cancel_report["lifecycle"]["counts"]["running"], 1);
    let cancel_job = &cancel_report["jobs"].as_array().expect("cancel jobs")[0];
    assert_eq!(cancel_job["status"], "running");
    assert_eq!(cancel_job["provider_batch_id"], "batch_test");
    assert_eq!(cancel_job["cancel_requested"], false);
    assert_eq!(
        cancel_job["cancel_operation"]["operation"],
        "POST https://api.openai.com/v1/batches/batch_test/cancel"
    );
    assert_eq!(
        cancel_job["cancel_operation"]["credential_env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        cancel_job["cancel_operation"]["auth"],
        "OPENAI_API_KEY=<redacted>"
    );

    let cancel_receipt = parse_json(
        &fs::read_to_string(&cancel_path).expect("read cancel receipt"),
        "cancel receipt",
    );
    assert_eq!(cancel_receipt["kind"], "harn.model_batch_cancel_receipt");
    assert_eq!(cancel_receipt["status"], "dry_run");
    assert_eq!(cancel_receipt["lifecycle"]["phase"], "cancel");
    assert_eq!(
        cancel_receipt["jobs"][0]["cancel_operation"]["credential_env"],
        "OPENAI_API_KEY"
    );

    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_test".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_output".to_string());
        jobs[0]["error_file_id"] = serde_json::Value::String("file_error".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize completed status"),
    )
    .expect("write completed status receipt");

    let skipped_cancel_path = tmp.path().join("cancel-completed.json");
    let skipped_cancel = run(
        &[
            "models",
            "batch",
            "cancel",
            "--receipt",
            status_path.to_str().expect("utf8 completed status path"),
            "--out",
            skipped_cancel_path
                .to_str()
                .expect("utf8 skipped cancel path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        skipped_cancel.exit_code, 0,
        "harn stderr={}",
        skipped_cancel.stderr
    );
    let skipped_cancel_value = parse_json(&skipped_cancel.stdout, "completed batch cancel");
    let skipped_cancel_report = success_data(&skipped_cancel_value);
    assert_eq!(skipped_cancel_report["skipped_count"], 1);
    assert_eq!(skipped_cancel_report["jobs"][0]["status"], "skipped");
    assert!(
        skipped_cancel_report["jobs"][0]["skip_reason"]
            .as_str()
            .unwrap_or("")
            .contains("terminal"),
        "skip reason={}",
        skipped_cancel_report["jobs"][0]["skip_reason"]
    );

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
            "--max-bytes",
            "1048576",
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["status"], "dry_run");
    assert_eq!(download_report["job_count"], 1);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 2);
    assert_eq!(download_report["lifecycle"]["phase"], "download");
    assert_eq!(download_report["lifecycle"]["state"], "dry_run");
    assert_eq!(download_report["lifecycle"]["counts"]["ready"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    assert_eq!(download_job["status"], "ready");
    assert_eq!(download_job["lifecycle"]["state"], "ready");
    assert_eq!(download_job["lifecycle"]["result_available"], false);
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_output");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["auth"],
        "OPENAI_API_KEY=<redacted>"
    );
    assert_eq!(artifacts[1]["label"], "error");

    let results_receipt_path = results_dir.join("receipt.json");
    let results_receipt = parse_json(
        &fs::read_to_string(results_receipt_path).expect("read results receipt"),
        "results receipt",
    );
    assert_eq!(results_receipt["kind"], "harn.model_batch_results_receipt");
    assert_eq!(results_receipt["status"], "dry_run");
    assert_eq!(results_receipt["lifecycle"]["phase"], "download");
    assert_eq!(results_receipt["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(results_receipt["artifactCount"], 2);

    status_receipt["status"] = serde_json::Value::String("canceled".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(0);
    status_receipt["canceledCount"] = serde_json::Value::from(1);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("canceled".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("canceled".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize canceled status"),
    )
    .expect("write canceled status receipt");

    let canceled_results_dir = tmp.path().join("canceled-results");
    let canceled_download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 canceled status path"),
            "--out-dir",
            canceled_results_dir
                .to_str()
                .expect("utf8 canceled results dir"),
            "--max-bytes",
            "1048576",
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        canceled_download.exit_code, 0,
        "harn stderr={}",
        canceled_download.stderr
    );
    let canceled_download_value = parse_json(&canceled_download.stdout, "canceled batch download");
    let canceled_download_report = success_data(&canceled_download_value);
    assert_eq!(canceled_download_report["dry_run"], true);
    assert_eq!(canceled_download_report["artifact_count"], 2);
    let canceled_download_job = &canceled_download_report["jobs"]
        .as_array()
        .expect("canceled download jobs")[0];
    assert_eq!(canceled_download_job["status"], "ready");
    assert_eq!(canceled_download_job["source_status"], "canceled");
    assert_eq!(canceled_download_job["artifacts"][0]["label"], "output");
}
