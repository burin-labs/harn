pub(super) use crate::test_util::cli::parse_json;

pub(super) use crate::test_util::process::{harn_e2e_binary, run_harn_e2e as run, HarnCliOutput};

pub(super) const LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION: u64 = 5;

pub(super) fn success_data(value: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(value["ok"], serde_json::Value::Bool(true));
    value["data"].as_object().expect("success envelope data");
    &value["data"]
}

pub(super) fn write_http_fixture(path: &std::path::Path, mocks: serde_json::Value) {
    std::fs::write(
        path,
        serde_json::to_string_pretty(&serde_json::json!({"mocks": mocks}))
            .expect("serialize HTTP fixture"),
    )
    .expect("write HTTP fixture");
}

pub(super) fn fixture_calls_diagnostic(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| format!("<unavailable: {error}>"))
}

/// Runs `harn models batch prepare` and returns the parsed success envelope.
pub(super) fn prepare_batch(
    manifest: &std::path::Path,
    out_dir: &std::path::Path,
    label: &str,
) -> serde_json::Value {
    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(
        prepared.exit_code, 0,
        "stdout={}\nstderr={}",
        prepared.stdout, prepared.stderr
    );
    parse_json(&prepared.stdout, label)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn batch_manifest(
    provider: &str,
    model: &str,
    workload: &str,
    tool_format: &str,
    wire_format: &str,
    input_mode: &str,
    request: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "kind": "harn.model_batch_manifest",
        "producer": "test",
        "workload": workload,
        "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
        "requestCount": 1,
        "groupCount": 1,
        "groups": [{
            "id": format!("{provider}-fixture"),
            "provider": provider,
            "model": model,
            "workload": workload,
            "endpoint": "provider_default",
            "tool_format": tool_format,
            "batch": {"api": true, "wire_format": wire_format, "input_mode": input_mode},
            "requests": [{
                "custom_id": format!("{provider}_1"),
                "source_line": 1,
                "source_sha256": "fixture",
                "metadata": {},
                "request": request,
            }],
        }],
        "warnings": [],
    })
}
