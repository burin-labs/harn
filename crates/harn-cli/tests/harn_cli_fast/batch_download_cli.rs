use std::fs;

use crate::test_util::process::{run_harn_e2e as run, HarnCliOutput};

fn parse_json(output: &HarnCliOutput, label: &str) -> serde_json::Value {
    serde_json::from_str(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout is not valid JSON: {error}\n{}",
            output.stdout
        )
    })
}

#[test]
fn accepts_each_optional_provider_result_file() {
    let cases = [
        ("openai", "openai", "output_file_id", "error_file_id"),
        ("mistral", "mistral", "output_file", "error_file"),
    ];

    for (provider, wire_format, output_field, error_field) in cases {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status_path = tmp.path().join("status.json");
        let job = serde_json::json!({
            "id": format!("{provider}-optional-result"),
            "provider": provider,
            "status": "completed",
            "batch": {"wire_format": wire_format},
        });
        for (label, present_field) in [("output", output_field), ("error", error_field)] {
            let mut one_result = job.clone();
            one_result[present_field] = serde_json::Value::String(format!("file-{label}"));
            let receipt = serde_json::json!({
                "schemaVersion": 1,
                "kind": "harn.model_batch_status_receipt",
                "status": "completed",
                "jobs": [one_result],
            });
            fs::write(
                &status_path,
                serde_json::to_string_pretty(&receipt).expect("serialize status receipt"),
            )
            .expect("write status receipt");

            let results_dir = tmp.path().join(format!("results-{label}"));
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
            assert_eq!(
                download.exit_code, 0,
                "{provider} {label} stderr={}",
                download.stderr
            );
            let value = parse_json(&download, "optional batch result download");
            let report = &value["data"];
            assert_eq!(value["ok"], true, "provider={provider}, payload={value}");
            assert_eq!(report["artifact_count"], 1, "provider={provider}");
            assert_eq!(report["jobs"][0]["artifacts"][0]["label"], label);
            assert_eq!(
                report["jobs"][0]["artifacts"][0]["handle"],
                format!("file-{label}")
            );
        }

        let receipt = serde_json::json!({
            "schemaVersion": 1,
            "kind": "harn.model_batch_status_receipt",
            "status": "completed",
            "jobs": [job],
        });
        fs::write(
            &status_path,
            serde_json::to_string_pretty(&receipt).expect("serialize handle-free receipt"),
        )
        .expect("write handle-free receipt");
        let results_dir = tmp.path().join("results-none");
        let no_results = run(
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
        assert_ne!(no_results.exit_code, 0, "provider={provider}");
        let error = parse_json(&no_results, "missing batch result handles");
        assert_eq!(error["ok"], false, "provider={provider}");
        assert!(
            error["error"]["details"]["errors"][0]
                .as_str()
                .unwrap_or("")
                .contains("has no provider result handles"),
            "provider={provider}, payload={error}"
        );
    }
}

#[test]
fn status_writer_round_trip_preserves_optional_result_handles() {
    let cases = [
        ("openai", "openai", "output_file_id"),
        ("mistral", "mistral", "output_file"),
    ];

    for (provider, wire_format, output_field) in cases {
        let tmp = tempfile::tempdir().expect("tempdir");
        let submission_path = tmp.path().join("submission.json");
        let status_path = tmp.path().join("status.json");
        let results_dir = tmp.path().join("results");
        let handle = format!("file-{provider}-output");
        let mut job = serde_json::json!({
            "id": format!("{provider}-completed-output-only"),
            "provider": provider,
            "status": "completed",
            "provider_status": "completed",
            "provider_batch_id": format!("batch-{provider}"),
            "batch": {"wire_format": wire_format},
        });
        job[output_field] = serde_json::Value::String(handle.clone());
        let submission = serde_json::json!({
            "schemaVersion": 1,
            "kind": "harn.model_batch_submission_receipt",
            "status": "completed",
            "jobs": [job],
        });
        fs::write(
            &submission_path,
            serde_json::to_string_pretty(&submission).expect("serialize submission receipt"),
        )
        .expect("write submission receipt");

        let status = run(
            &[
                "models",
                "batch",
                "status",
                "--submission",
                submission_path.to_str().expect("UTF-8 submission path"),
                "--out",
                status_path.to_str().expect("UTF-8 status path"),
                "--dry-run",
                "--json",
            ],
            &[],
        );
        assert_eq!(
            status.exit_code, 0,
            "provider={provider}: {}",
            status.stderr
        );
        let status_envelope = parse_json(&status, "batch status round trip");
        assert_eq!(status_envelope["ok"], true, "provider={provider}");
        let persisted = serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(&status_path).expect("read persisted status receipt"),
        )
        .expect("parse persisted status receipt");
        assert_eq!(
            persisted["jobs"][0][output_field], handle,
            "status writer dropped provider={provider} handle"
        );

        let download = run(
            &[
                "models",
                "batch",
                "download",
                "--status",
                status_path.to_str().expect("UTF-8 status path"),
                "--out-dir",
                results_dir.to_str().expect("UTF-8 results dir"),
                "--dry-run",
                "--json",
            ],
            &[],
        );
        assert_eq!(
            download.exit_code, 0,
            "provider={provider}: {}",
            download.stderr
        );
        let downloaded = parse_json(&download, "batch download round trip");
        assert_eq!(downloaded["ok"], true, "provider={provider}");
        assert_eq!(
            downloaded["data"]["artifact_count"], 1,
            "provider={provider}"
        );
        assert_eq!(
            downloaded["data"]["jobs"][0]["artifacts"][0]["handle"], handle,
            "download reader changed provider={provider} handle"
        );

        let mut missing_handle = persisted;
        missing_handle["jobs"][0]
            .as_object_mut()
            .expect("persisted job is an object")
            .remove(output_field);
        fs::write(
            &status_path,
            serde_json::to_string_pretty(&missing_handle).expect("serialize negative control"),
        )
        .expect("write handle-free status receipt");
        let rejected = run(
            &[
                "models",
                "batch",
                "download",
                "--status",
                status_path.to_str().expect("UTF-8 status path"),
                "--out-dir",
                results_dir.to_str().expect("UTF-8 results dir"),
                "--dry-run",
                "--json",
            ],
            &[],
        );
        assert_ne!(rejected.exit_code, 0, "provider={provider}");
        let rejected_payload = parse_json(&rejected, "missing result handle negative control");
        assert_eq!(rejected_payload["ok"], false, "provider={provider}");
    }
}
