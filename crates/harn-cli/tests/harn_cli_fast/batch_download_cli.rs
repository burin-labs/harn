use std::{fs, path::Path};

use crate::test_util::process::{run_harn_e2e as run, HarnCliOutput};

const PROVIDERS: [(&str, &str, &str); 2] = [
    ("openai", "output_file_id", "error_file_id"),
    ("mistral", "output_file", "error_file"),
];

fn parse_json(output: &HarnCliOutput, label: &str) -> serde_json::Value {
    serde_json::from_str(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout is not valid JSON: {error}\n{}",
            output.stdout
        )
    })
}

fn download(status_path: &Path, results_dir: &Path) -> HarnCliOutput {
    run(
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
    )
}

fn write_receipt(path: &Path, receipt: &serde_json::Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
}

fn assert_missing_handles(output: &HarnCliOutput, job_ids: &[String]) {
    assert_ne!(output.exit_code, 0, "handle-free download succeeded");
    let payload = parse_json(output, "missing batch result handles");
    assert_eq!(payload["ok"], false, "payload={payload}");
    let errors = payload["error"]["details"]["errors"]
        .as_array()
        .expect("errors array");
    assert_eq!(errors.len(), job_ids.len(), "payload={payload}");
    for id in job_ids {
        let expected = format!("job {id} has no provider result handles");
        assert!(
            errors
                .iter()
                .any(|error| error.as_str() == Some(expected.as_str())),
            "missing {expected}: {payload}"
        );
    }
}

#[test]
fn accepts_each_optional_provider_result_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let status_path = tmp.path().join("status.json");
    let mut jobs = Vec::new();
    let mut missing_jobs = Vec::new();
    let mut expected = Vec::new();
    for (provider, output_field, error_field) in PROVIDERS {
        let job = serde_json::json!({
            "id": format!("{provider}-optional-result"),
            "provider": provider,
            "status": "completed",
            "batch": {"wire_format": provider},
        });
        missing_jobs.push(job.clone());
        for (label, field) in [("output", output_field), ("error", error_field)] {
            let id = format!("{provider}-{label}");
            let handle = format!("file-{provider}-{label}");
            let mut result = job.clone();
            result["id"] = id.clone().into();
            result[field] = handle.clone().into();
            jobs.push(result);
            expected.push((id, label, handle));
        }
    }
    let mut receipt = serde_json::json!({
        "schemaVersion": 1,
        "kind": "harn.model_batch_status_receipt",
        "status": "completed",
        "jobs": jobs,
    });
    write_receipt(&status_path, &receipt);
    let output = download(&status_path, &tmp.path().join("results"));
    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    let value = parse_json(&output, "optional batch result download");
    assert_eq!(value["ok"], true, "payload={value}");
    let report = &value["data"];
    assert_eq!(report["artifact_count"], expected.len());
    let downloaded = report["jobs"].as_array().expect("downloaded jobs");
    assert_eq!(downloaded.len(), expected.len());
    for (id, label, handle) in expected {
        let job = downloaded
            .iter()
            .find(|job| job["id"] == id)
            .expect("downloaded job");
        assert_eq!(
            job["artifacts"].as_array().expect("artifacts").len(),
            1,
            "job={id}"
        );
        assert_eq!(job["artifacts"][0]["label"], label, "job={id}");
        assert_eq!(job["artifacts"][0]["handle"], handle, "job={id}");
    }
    let missing_ids = missing_jobs
        .iter()
        .map(|job| job["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    receipt["jobs"] = missing_jobs.into();
    write_receipt(&status_path, &receipt);
    assert_missing_handles(
        &download(&status_path, &tmp.path().join("results-none")),
        &missing_ids,
    );
}

#[test]
fn status_writer_round_trip_preserves_optional_result_handles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let submission_path = tmp.path().join("submission.json");
    let status_path = tmp.path().join("status.json");
    let results_dir = tmp.path().join("results");
    let jobs = PROVIDERS.map(|(provider, output_field, _)| {
        let mut job = serde_json::json!({
            "id": format!("{provider}-completed-output-only"),
            "provider": provider,
            "status": "completed",
            "provider_status": "completed",
            "provider_batch_id": format!("batch-{provider}"),
            "batch": {"wire_format": provider},
        });
        job[output_field] = format!("file-{provider}-output").into();
        job
    });
    write_receipt(
        &submission_path,
        &serde_json::json!({
            "schemaVersion": 1,
            "kind": "harn.model_batch_submission_receipt",
            "status": "completed",
            "jobs": jobs,
        }),
    );
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
    assert_eq!(status.exit_code, 0, "{}", status.stderr);
    assert_eq!(parse_json(&status, "batch status round trip")["ok"], true);
    let mut persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&status_path).expect("read persisted status receipt"),
    )
    .expect("parse persisted status receipt");
    assert_eq!(
        persisted["jobs"].as_array().expect("persisted jobs").len(),
        PROVIDERS.len()
    );
    let output = download(&status_path, &results_dir);
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    let downloaded = parse_json(&output, "batch download round trip");
    assert_eq!(downloaded["ok"], true);
    assert_eq!(downloaded["data"]["artifact_count"], PROVIDERS.len());
    let downloaded_jobs = downloaded["data"]["jobs"]
        .as_array()
        .expect("downloaded jobs");
    assert_eq!(downloaded_jobs.len(), PROVIDERS.len());
    let mut missing_ids = Vec::new();
    for (provider, output_field, _) in PROVIDERS {
        let id = format!("{provider}-completed-output-only");
        let handle = format!("file-{provider}-output");
        let job = persisted["jobs"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|job| job["id"] == id)
            .expect("persisted provider job");
        assert_eq!(
            job[output_field], handle,
            "status writer dropped provider={provider} handle"
        );
        let downloaded_job = downloaded_jobs
            .iter()
            .find(|job| job["id"] == id)
            .expect("downloaded provider job");
        assert_eq!(
            downloaded_job["artifacts"][0]["handle"], handle,
            "download reader changed provider={provider} handle"
        );
        job.as_object_mut().unwrap().remove(output_field);
        missing_ids.push(id);
    }
    write_receipt(&status_path, &persisted);
    assert_missing_handles(&download(&status_path, &results_dir), &missing_ids);
}
