use crate::test_util::process::harn_e2e_command;

use std::fs;

#[test]
fn one_shot_worker_calls_a_manifest_connector_with_its_declared_secret() {
    let project = tempfile::tempdir().expect("temp project");
    fs::write(
        project.path().join("harn.toml"),
        r#"
[package]
name = "worker-connector-smoke"
version = "0.1.0"

[[providers]]
id = "worker-echo"
connector = { harn = "echo_connector.harn" }

[providers.setup]
required_secrets = ["worker-echo/api-token"]
"#,
    )
    .expect("manifest");
    fs::write(
        project.path().join("echo_connector.harn"),
        r#"
pub fn provider_id() { return "worker-echo" }
pub fn kinds() { return ["job"] }
pub fn payload_schema() {
  return {harn_schema_name: "WorkerEchoPayload", json_schema: {type: "object", additionalProperties: true}}
}
pub fn init(_harness: Harness, _ctx) {}
pub fn activate(_harness: Harness, _bindings) {}
pub fn normalize_inbound(_harness: Harness, _raw) { throw "inbound_not_supported" }
pub fn call(_harness: Harness, method, args) {
  const token = args.secrets.api_token
  return {method: method, credential_resolved: len(token) > 0}
}
"#,
    )
    .expect("connector module");
    let script = project.path().join("job.harn");
    fs::write(
        &script,
        r#"
import "std/triggers"

@job("connector_smoke")
@retry(max: 1, backoff: "linear")
pub fn connector_smoke(harness: Harness, _event: TriggerEvent) -> dict {
  return harness.net.connector_call("worker-echo", "ping", {})
}
"#,
    )
    .expect("job source");
    let request = project.path().join("request.json");
    fs::write(&request, "{}").expect("request");

    let run = |credential: Option<&str>| {
        let mut command = harn_e2e_command();
        command
            .current_dir(project.path())
            .env("HARN_SECRET_PROVIDERS", "env")
            .env_remove("HARN_SECRET_WORKER_ECHO_API_TOKEN")
            .args([
                "run",
                script.to_str().expect("UTF-8 script path"),
                "--as-job",
                "--job",
                "connector_smoke",
                "--request",
                request.to_str().expect("UTF-8 request path"),
            ]);
        if let Some(credential) = credential {
            command.env("HARN_SECRET_WORKER_ECHO_API_TOKEN", credential);
        }
        command.output().expect("run one-shot worker")
    };

    let output = run(Some("configured-for-test"));

    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("worker JSON report");
    assert_eq!(report["method"], "ping");
    assert_eq!(report["credential_resolved"], true);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains("configured-for-test"));

    fs::write(
        project.path().join("harn.toml"),
        "[package]\nname = \"worker-connector-smoke\"\nversion = \"0.1.0\"\n",
    )
    .expect("manifest without connector");
    let missing_connector = run(None);
    let missing_connector_error = String::from_utf8_lossy(&missing_connector.stdout).to_string();
    assert!(!missing_connector.status.success());
    assert!(missing_connector_error.contains("connector `worker-echo` is not active"));

    fs::write(
        project.path().join("harn.toml"),
        r#"
[package]
name = "worker-connector-smoke"
version = "0.1.0"

[[providers]]
id = "worker-echo"
connector = { harn = "echo_connector.harn" }
"#,
    )
    .expect("manifest with connector");
    let connector = fs::read_to_string(project.path().join("echo_connector.harn"))
        .expect("read connector module");
    fs::write(
        project.path().join("echo_connector.harn"),
        connector.replace(
            "pub fn init(_harness: Harness, _ctx) {}",
            "pub fn init(_harness: Harness, _ctx) { throw \"fixture init refusal\" }",
        ),
    )
    .expect("connector that refuses initialization");
    let init_failure = run(None);
    let init_error = String::from_utf8_lossy(&init_failure.stderr);
    assert!(!init_failure.status.success());
    assert!(init_error.contains("failed to initialize worker connector"));
    assert!(init_error.contains("fixture init refusal"));
    assert!(!init_error.contains("is not active"));

    fs::write(project.path().join("echo_connector.harn"), &connector)
        .expect("restore connector module");
    fs::write(
        project.path().join("harn.toml"),
        r#"
[package]
name = "worker-connector-smoke"
version = "0.1.0"

[[providers]]
id = "worker-echo"
connector = { harn = "echo_connector.harn" }

[providers.setup]
required_secrets = ["worker-echo/api-token"]
"#,
    )
    .expect("manifest with declared credential");
    let missing_credential = run(None);
    let credential_error = format!(
        "{}{}",
        String::from_utf8_lossy(&missing_credential.stdout),
        String::from_utf8_lossy(&missing_credential.stderr)
    );
    assert!(!missing_credential.status.success());
    assert!(credential_error.contains("api_token"));
    assert!(!credential_error.contains("failed to initialize worker connector"));
    assert!(!credential_error.contains("is not active"));
}
