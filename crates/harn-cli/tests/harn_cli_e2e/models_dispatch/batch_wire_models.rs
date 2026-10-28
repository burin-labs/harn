use std::fs;

use super::support::{
    fixture_calls_diagnostic, parse_json, prepare_batch, run, success_data, write_http_fixture,
};

#[test]
fn models_batch_mistral_live_create_uses_prepared_wire_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests = tmp.path().join("requests.jsonl");
    let manifest = tmp.path().join("manifest.json");
    let prepared_dir = tmp.path().join("prepared");
    fs::write(&requests, "{\"custom_id\":\"mistral-1\",\"messages\":[{\"role\":\"user\",\"content\":\"grade\"}],\"max_tokens\":16}\n")
        .expect("write requests");
    let planned = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "mistral",
            "--model",
            "mistral-large-4-0",
            "--requests",
            requests.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--json",
        ],
        &[],
    );
    assert_eq!(planned.exit_code, 0, "{}", planned.stderr);
    let prepared_json = prepare_batch(&manifest, &prepared_dir, "Mistral prepare");
    let job = &success_data(&prepared_json)["jobs"][0];
    assert_eq!(job["model"], "mistral-large-4-0");
    assert_eq!(job["wire_model"], "mistral-large-4");
    assert_eq!(job["submit"]["create_job"]["model"], "mistral-large-4");
    let line = fs::read_to_string(job["request_file"].as_str().unwrap()).unwrap();
    assert_eq!(
        parse_json(line.trim(), "Mistral request")["body"]["model"],
        "mistral-large-4"
    );
    let fixture = tmp.path().join("http.json");
    let calls = tmp.path().join("calls.json");
    write_http_fixture(
        &fixture,
        serde_json::json!([
            {"method":"POST", "url":"https://api.mistral.ai/v1/files",
             "response":{"status":200,"body":"{\"id\":\"file-mistral\"}"}},
            {"method":"POST", "url":"https://api.mistral.ai/v1/batch/jobs",
             "response":{"status":200,"body":"{\"id\":\"batch-mistral\",\"status\":\"QUEUED\"}"}}
        ]),
    );
    let output = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            prepared_dir.join("receipt.json").to_str().unwrap(),
            "--out",
            output.to_str().unwrap(),
            "--json",
        ],
        &[
            ("MISTRAL_API_KEY", "fixture-secret"),
            (
                "HARN_MODELS_BATCH_TEST_HTTP_FIXTURE",
                fixture.to_str().unwrap(),
            ),
            ("HARN_MODELS_BATCH_TEST_HTTP_CALLS", calls.to_str().unwrap()),
        ],
    );
    assert_eq!(
        submitted.exit_code,
        0,
        "stdout={} stderr={} calls={}",
        submitted.stdout,
        submitted.stderr,
        fixture_calls_diagnostic(&calls)
    );
    let recorded = parse_json(&fs::read_to_string(&calls).unwrap(), "Mistral calls");
    assert_eq!(recorded.as_array().unwrap().len(), 2);
    let create_body = parse_json(recorded[1]["body"].as_str().unwrap(), "Mistral create");
    assert_eq!(create_body["model"], "mistral-large-4");
    let receipt = parse_json(&fs::read_to_string(output).unwrap(), "Mistral submission");
    assert_eq!(receipt["jobs"][0]["model"], "mistral-large-4-0");
}
