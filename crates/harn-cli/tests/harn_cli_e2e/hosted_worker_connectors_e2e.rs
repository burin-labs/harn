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

@job("cross_tenant")
@retry(max: 1, backoff: "linear")
pub fn cross_tenant(harness: Harness, _event: TriggerEvent) -> dict {
  const token = harness.secrets.read("harn.tenant.tenant-b.worker-echo/api-token")
  return {credential_resolved: len(token) > 0}
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

    let tenant_state = project.path().join(".harn/orchestrator");
    for tenant in ["tenant-a", "tenant-b"] {
        let output = harn_e2e_command()
            .current_dir(project.path())
            .args([
                "orchestrator",
                "tenant",
                "--state-dir",
                tenant_state.to_str().expect("UTF-8 tenant state path"),
                "create",
                tenant,
            ])
            .output()
            .expect("create test tenant");
        assert!(
            output.status.success(),
            "failed to create {tenant}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let run_tenant = |job: &str, tenant: &str, tenant_a: Option<&str>, tenant_b: Option<&str>| {
        let mut command = harn_e2e_command();
        command
            .current_dir(project.path())
            .env("HARN_SECRET_PROVIDERS", "env")
            .env_remove("HARN_SECRET_WORKER_ECHO_API_TOKEN")
            .env_remove("HARN_SECRET_HARN_TENANT_TENANT_A_WORKER_ECHO_API_TOKEN")
            .env_remove("HARN_SECRET_HARN_TENANT_TENANT_B_WORKER_ECHO_API_TOKEN")
            .args([
                "run",
                script.to_str().expect("UTF-8 script path"),
                "--as-job",
                "--job",
                job,
                "--request",
                request.to_str().expect("UTF-8 request path"),
                "--tenant",
                tenant,
                "--tenant-state-dir",
                tenant_state.to_str().expect("UTF-8 tenant state path"),
            ]);
        if let Some(value) = tenant_a {
            command.env(
                "HARN_SECRET_HARN_TENANT_TENANT_A_WORKER_ECHO_API_TOKEN",
                value,
            );
        }
        if let Some(value) = tenant_b {
            command.env(
                "HARN_SECRET_HARN_TENANT_TENANT_B_WORKER_ECHO_API_TOKEN",
                value,
            );
        }
        command.output().expect("run tenanted worker")
    };

    let tenant_b = run_tenant(
        "connector_smoke",
        "tenant-b",
        None,
        Some("tenant-b-only-value"),
    );
    assert!(
        tenant_b.status.success(),
        "tenant-b failed: {}",
        String::from_utf8_lossy(&tenant_b.stderr)
    );
    let tenant_a_missing = run_tenant(
        "connector_smoke",
        "tenant-a",
        None,
        Some("tenant-b-only-value"),
    );
    let tenant_a_missing_error = format!(
        "{}{}",
        String::from_utf8_lossy(&tenant_a_missing.stdout),
        String::from_utf8_lossy(&tenant_a_missing.stderr)
    );
    assert!(!tenant_a_missing.status.success());
    assert!(tenant_a_missing_error.contains("api_token"));
    assert!(!tenant_a_missing_error.contains("denied"));

    let tenant_a_denied = run_tenant(
        "cross_tenant",
        "tenant-a",
        Some("tenant-a-only-value"),
        Some("tenant-b-only-value"),
    );
    let tenant_a_denied_error = format!(
        "{}{}",
        String::from_utf8_lossy(&tenant_a_denied.stdout),
        String::from_utf8_lossy(&tenant_a_denied.stderr)
    );
    assert!(!tenant_a_denied.status.success());
    assert!(tenant_a_denied_error.contains("denied"));
    assert!(!tenant_a_denied_error.contains("not found"));
    assert!(!tenant_a_denied_error.contains("tenant-a-only-value"));
    assert!(!tenant_a_denied_error.contains("tenant-b-only-value"));

    assert_tree_omits(project.path(), b"tenant-a-only-value");
    assert_tree_omits(project.path(), b"tenant-b-only-value");
}

fn assert_tree_omits(root: &std::path::Path, needle: &[u8]) {
    for entry in fs::read_dir(root).expect("read durable test tree") {
        let entry = entry.expect("durable test entry");
        let path = entry.path();
        if path.is_dir() {
            assert_tree_omits(&path, needle);
        } else {
            let bytes = fs::read(&path).expect("read durable test file");
            assert!(!bytes.windows(needle.len()).any(|window| window == needle));
        }
    }
}
