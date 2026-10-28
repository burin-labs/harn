use std::fs;

use super::support::{
    batch_manifest, fixture_calls_diagnostic, parse_json, prepare_batch, run, success_data,
    write_http_fixture,
};

#[test]
fn models_batch_azure_openai_live_fixture_covers_submit_cancel_status_and_download() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let prepared_dir = tmp.path().join("prepared");
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
  "groups": [{
    "id": "azure-fixture",
    "provider": "azure_openai",
    "model": "gpt-5-deployment",
    "workload": "eval",
    "endpoint": "/v1/chat/completions",
    "tool_format": "native",
    "batch": {"api": true, "wire_format": "openai", "input_mode": "jsonl_file"},
    "requests": [{
      "custom_id": "azure_1",
      "source_line": 1,
      "source_sha256": "fixture",
      "metadata": {},
      "request": {"messages": [{"role": "user", "content": "grade"}], "max_tokens": 16}
    }]
  }],
  "warnings": []
}"#,
    )
    .expect("write Azure manifest");
    let prepared_json = prepare_batch(&manifest_path, &prepared_dir, "Azure prepare");
    let prepared_data = success_data(&prepared_json);
    let prepare_receipt = prepared_data["receipt"].as_str().expect("prepare receipt");
    let request_file = prepared_data["jobs"][0]["request_file"]
        .as_str()
        .expect("request file");
    let request_line = fs::read_to_string(request_file).expect("read Azure request");
    let request = parse_json(
        request_line.lines().next().expect("request line"),
        "Azure row",
    );
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "gpt-5-deployment");

    let fixture = tmp.path().join("http.json");
    let calls = tmp.path().join("calls.json");
    write_http_fixture(
        &fixture,
        serde_json::json!([
            {
                "method": "POST",
                "url": "https://azure.example.test/openai/v1/files",
                "response": {"status": 200, "body": "{\"id\":\"az-file\"}"}
            },
            {
                "method": "POST",
                "url": "https://azure.example.test/openai/v1/batches",
                "response": {"status": 200, "body": "{\"id\":\"az-batch\",\"status\":\"validating\"}"}
            }
        ]),
    );
    let submission = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            prepare_receipt,
            "--out",
            submission.to_str().expect("submission path"),
            "--json",
        ],
        &[
            (
                "AZURE_OPENAI_ENDPOINT",
                "https://azure.example.test/openai/v1/",
            ),
            ("AZURE_OPENAI_API_KEY", "azure-secret"),
            (
                "HARN_MODELS_BATCH_TEST_HTTP_FIXTURE",
                fixture.to_str().expect("fixture path"),
            ),
            (
                "HARN_MODELS_BATCH_TEST_HTTP_CALLS",
                calls.to_str().expect("calls path"),
            ),
        ],
    );
    assert_eq!(
        submitted.exit_code,
        0,
        "stdout={}\nstderr={}\nfixture calls={}",
        submitted.stdout,
        submitted.stderr,
        fixture_calls_diagnostic(&calls)
    );
    let submitted_json = parse_json(&submitted.stdout, "Azure submit");
    let submitted_job = &success_data(&submitted_json)["jobs"][0];
    assert_eq!(submitted_job["provider_batch_id"], "az-batch");
    assert!(
        !fs::read_to_string(&submission)
            .expect("submission text")
            .contains("azure-secret"),
        "Azure secret leaked into submission receipt"
    );
    let submit_calls = parse_json(&fs::read_to_string(&calls).expect("calls"), "submit calls");
    assert_eq!(submit_calls[0]["headers"]["api-key"], "[redacted]");
    assert_eq!(submit_calls[1]["headers"]["api-key"], "[redacted]");
    assert!(submit_calls[1]["headers"]["authorization"].is_null());
    assert_eq!(
        parse_json(
            submit_calls[1]["body"].as_str().expect("create body"),
            "create body"
        )["endpoint"],
        "/v1/chat/completions"
    );

    write_http_fixture(
        &fixture,
        serde_json::json!([{
            "method": "POST",
            "url": "https://azure.example.test/openai/v1/batches/az-batch/cancel",
            "response": {"status": 200, "body": "{\"id\":\"az-batch\",\"status\":\"cancelling\"}"}
        }]),
    );
    let cancellation = tmp.path().join("cancel.json");
    let canceled = run(
        &[
            "models",
            "batch",
            "cancel",
            "--receipt",
            submission.to_str().expect("submission path"),
            "--out",
            cancellation.to_str().expect("cancel path"),
            "--json",
        ],
        &[
            ("AZURE_OPENAI_ENDPOINT", "https://azure.example.test"),
            ("AZURE_OPENAI_API_KEY", "azure-secret"),
            (
                "HARN_MODELS_BATCH_TEST_HTTP_FIXTURE",
                fixture.to_str().expect("fixture path"),
            ),
        ],
    );
    assert_eq!(canceled.exit_code, 0, "stderr={}", canceled.stderr);

    write_http_fixture(
        &fixture,
        serde_json::json!([{
            "method": "GET",
            "url": "https://azure.example.test/openai/v1/batches/az-batch",
            "response": {"status": 200, "body": "{\"id\":\"az-batch\",\"status\":\"completed\",\"output_file_id\":\"az-output\"}"}
        }]),
    );
    let status = tmp.path().join("status.json");
    let polled = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission.to_str().expect("submission path"),
            "--out",
            status.to_str().expect("status path"),
            "--json",
        ],
        &[
            ("AZURE_OPENAI_ENDPOINT", "https://azure.example.test"),
            ("AZURE_OPENAI_API_KEY", "azure-secret"),
            (
                "HARN_MODELS_BATCH_TEST_HTTP_FIXTURE",
                fixture.to_str().expect("fixture path"),
            ),
        ],
    );
    assert_eq!(polled.exit_code, 0, "stderr={}", polled.stderr);

    write_http_fixture(
        &fixture,
        serde_json::json!([{
            "method": "GET",
            "url": "https://azure.example.test/openai/v1/files/az-output/content",
            "response": {"status": 200, "body": "{\"custom_id\":\"azure_1\",\"response\":{\"status_code\":200,\"body\":{}}}\n"}
        }]),
    );
    let results = tmp.path().join("results");
    let downloaded = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status.to_str().expect("status path"),
            "--out-dir",
            results.to_str().expect("results path"),
            "--json",
        ],
        &[
            ("AZURE_OPENAI_ENDPOINT", "https://azure.example.test"),
            ("AZURE_OPENAI_API_KEY", "azure-secret"),
            (
                "HARN_MODELS_BATCH_TEST_HTTP_FIXTURE",
                fixture.to_str().expect("fixture path"),
            ),
        ],
    );
    assert_eq!(downloaded.exit_code, 0, "stderr={}", downloaded.stderr);
    assert_eq!(
        success_data(&parse_json(&downloaded.stdout, "Azure download"))["artifact_count"],
        1
    );
}

#[test]
fn models_batch_bedrock_live_fixture_covers_signed_lifecycle_and_rejoin_wire() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let prepared_dir = tmp.path().join("prepared");
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
  "groups": [{
    "id": "bedrock-fixture",
    "provider": "bedrock",
    "model": "anthropic.claude-sonnet-4-5-20250929-v1:0",
    "workload": "eval",
    "endpoint": "provider_default",
    "tool_format": "native",
    "provider_policy": {
      "region": "us-west-2",
      "role_arn": "arn:aws:iam::123456789012:role/HarnBatch",
      "input_s3_uri": "s3://fixture-bucket/input/requests.jsonl",
      "output_s3_uri": "s3://fixture-bucket/output/results.jsonl"
    },
    "batch": {
      "api": true,
      "wire_format": "bedrock",
      "input_mode": "jsonl_file",
      "regions": ["us-east-1", "us-west-2"]
    },
    "requests": [{
      "custom_id": "bedrock_1",
      "source_line": 1,
      "source_sha256": "fixture",
      "metadata": {},
      "request": {"anthropic_version": "bedrock-2023-05-31", "max_tokens": 16, "messages": [{"role": "user", "content": "grade"}]}
    }]
  }],
  "warnings": []
}"#,
    )
    .expect("write Bedrock manifest");
    let prepared_json = prepare_batch(&manifest_path, &prepared_dir, "Bedrock prepare");
    let prepared_data = success_data(&prepared_json);
    let job = &prepared_data["jobs"][0];
    assert_eq!(job["create_recovery"]["mode"], "deterministic_token");
    let request_text =
        fs::read_to_string(job["request_file"].as_str().expect("request file")).expect("request");
    let request = parse_json(
        request_text.lines().next().expect("request line"),
        "Bedrock row",
    );
    assert_eq!(request["recordId"], "bedrock_1");
    assert!(request["modelInput"]["model"].is_null());

    let fixture = tmp.path().join("http.json");
    let calls = tmp.path().join("calls.json");
    write_http_fixture(
        &fixture,
        serde_json::json!([
            {
                "method": "PUT",
                "url": "https://s3.example.test/fixture-bucket/input/requests.jsonl",
                "response": {"status": 200, "body": ""}
            },
            {
                "method": "POST",
                "url": "https://bedrock.example.test/model-invocation-job",
                "response": {"status": 200, "body": "{\"jobArn\":\"arn:aws:bedrock:us-west-2:123456789012:model-invocation-job/job-1\",\"status\":\"Submitted\"}"}
            }
        ]),
    );
    let submission = tmp.path().join("submission.json");
    let aws_env = [
        ("AWS_REGION", "us-west-2"),
        ("AWS_ACCESS_KEY_ID", "AKIATESTFIXTURE"),
        ("AWS_SECRET_ACCESS_KEY", "fixture-secret-key"),
        (
            "HARN_BATCH_BEDROCK_BASE_URL",
            "https://bedrock.example.test",
        ),
        ("HARN_BATCH_S3_BASE_URL", "https://s3.example.test"),
        (
            "HARN_MODELS_BATCH_TEST_HTTP_FIXTURE",
            fixture.to_str().expect("fixture path"),
        ),
        (
            "HARN_MODELS_BATCH_TEST_HTTP_CALLS",
            calls.to_str().expect("calls path"),
        ),
    ];
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            prepared_data["receipt"].as_str().expect("receipt"),
            "--out",
            submission.to_str().expect("submission path"),
            "--json",
        ],
        &aws_env,
    );
    assert_eq!(
        submitted.exit_code,
        0,
        "stdout={}\nstderr={}\nfixture calls={}",
        submitted.stdout,
        submitted.stderr,
        fixture_calls_diagnostic(&calls)
    );
    let submission_text = fs::read_to_string(&submission).expect("submission");
    assert!(!submission_text.contains("fixture-secret-key"));
    assert!(!submission_text.contains("AKIATESTFIXTURE"));
    let submission_json = parse_json(&submitted.stdout, "Bedrock submit");
    let submitted_job = &success_data(&submission_json)["jobs"][0];
    assert_eq!(
        submitted_job["client_request_token"]
            .as_str()
            .expect("token")
            .len(),
        64
    );
    let submit_calls = parse_json(&fs::read_to_string(&calls).expect("calls"), "Bedrock calls");
    assert_eq!(submit_calls[0]["headers"]["authorization"], "[redacted]");
    assert!(submit_calls[0]["headers"]["x-amz-date"].is_string());
    let create_body = parse_json(
        submit_calls[1]["body"].as_str().expect("create body"),
        "Bedrock create body",
    );
    assert_eq!(
        create_body["roleArn"],
        "arn:aws:iam::123456789012:role/HarnBatch"
    );
    assert_eq!(
        create_body["clientRequestToken"],
        submitted_job["client_request_token"]
    );

    let handle = submitted_job["provider_batch_handle"]
        .as_str()
        .expect("provider handle");
    write_http_fixture(
        &fixture,
        serde_json::json!([{
            "method": "POST",
            "url": format!("https://bedrock.example.test/model-invocation-job/{handle}/stop"),
            "response": {"status": 200, "body": "{\"status\":\"Stopping\"}"}
        }]),
    );
    let cancellation = tmp.path().join("cancel.json");
    let canceled = run(
        &[
            "models",
            "batch",
            "cancel",
            "--receipt",
            submission.to_str().expect("submission path"),
            "--out",
            cancellation.to_str().expect("cancel path"),
            "--json",
        ],
        &aws_env,
    );
    assert_eq!(canceled.exit_code, 0, "stderr={}", canceled.stderr);

    write_http_fixture(
        &fixture,
        serde_json::json!([{
            "method": "GET",
            "url": format!("https://bedrock.example.test/model-invocation-job/{handle}"),
            "response": {
                "status": 200,
                "body": "{\"status\":\"Completed\",\"outputDataConfig\":{\"s3OutputDataConfig\":{\"s3Uri\":\"s3://fixture-bucket/output/results.jsonl\"}}}"
            }
        }]),
    );
    let status = tmp.path().join("status.json");
    let polled = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission.to_str().expect("submission path"),
            "--out",
            status.to_str().expect("status path"),
            "--json",
        ],
        &aws_env,
    );
    assert_eq!(polled.exit_code, 0, "stderr={}", polled.stderr);

    write_http_fixture(
        &fixture,
        serde_json::json!([{
            "method": "GET",
            "url": "https://s3.example.test/fixture-bucket/output/results.jsonl",
            "response": {"status": 200, "body": "{\"recordId\":\"bedrock_1\",\"modelOutput\":{\"outputText\":\"fixture\"}}\n"}
        }]),
    );
    let results = tmp.path().join("results");
    let downloaded = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status.to_str().expect("status path"),
            "--out-dir",
            results.to_str().expect("results path"),
            "--json",
        ],
        &aws_env,
    );
    assert_eq!(downloaded.exit_code, 0, "stderr={}", downloaded.stderr);
    assert_eq!(
        success_data(&parse_json(&downloaded.stdout, "Bedrock download"))["artifact_count"],
        1
    );
}
#[test]
fn models_batch_prepare_anthropic_inline_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    let manifest = batch_manifest(
        "anthropic",
        "claude-haiku-4-5-20251001",
        "judge",
        "native",
        "anthropic_messages",
        "inline_requests",
        serde_json::json!({"max_tokens": 32, "messages": [{"role": "user", "content": "label this"}]}),
    );
    fs::write(&manifest_path, manifest.to_string()).expect("write manifest");

    let prepared_value = prepare_batch(&manifest_path, &out_dir, "batch prepare");
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
    assert_eq!(request_value["requests"][0]["custom_id"], "anthropic_1");
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

    let prepared_value = prepare_batch(&manifest_path, &out_dir, "batch prepare");
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
