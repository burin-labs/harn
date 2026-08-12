use super::*;

use std::fs;
use std::sync::OnceLock;

async fn connector_check_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn write_package(manifest_tail: &str, lib: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("harn.toml"),
        format!(
            r#"
[package]
name = "contract-test"
version = "0.1.0"

[[providers]]
id = "echo"
connector = {{ harn = "./lib.harn" }}

[providers.service]
name = "Echo"
description = "Sends and receives deterministic test messages."

[[providers.service.operations]]
id = "messages.read"
capability = "messages"
purpose = "Read messages from Echo."
effect = "read"
environments = ["mock", "test", "live"]

[providers.setup]
auth_type = "api-key"
flow = "api-key"
required_secrets = ["echo/api-token"]
setup_command = ["harn", "connect", "echo"]
validation_command = ["harn", "connect", "status", "--connector", "echo", "--json"]

[[providers.setup.health_checks]]
id = "api-token"
kind = "secret"
secret = "echo/api-token"

[providers.setup.recovery]
missing_auth = "Store echo/api-token."
expired_credentials = "Rotate echo/api-token."
revoked_credentials = "Replace echo/api-token."
missing_scopes = "Use an API key with the required scopes."
inaccessible_resource = "Grant access to the target echo resource."
transient_provider_outage = "Retry after the provider is reachable."

{manifest_tail}
"#
        ),
    )
    .unwrap();
    fs::write(dir.path().join("lib.harn"), lib).unwrap();
    dir
}

fn check_args(path: &Path) -> ConnectorCheckArgs {
    ConnectorCheckArgs {
        package: path.display().to_string(),
        providers: Vec::new(),
        run_poll_tick: false,
        json: false,
    }
}

#[test]
fn package_dir_from_anchor_finds_manifest_for_nested_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src/nested")).unwrap();
    fs::write(dir.path().join("harn.toml"), "[package]\nname = \"demo\"\n").unwrap();
    let nested = dir.path().join("src/nested/lib.harn");
    fs::write(&nested, "").unwrap();

    assert_eq!(package_dir_from_anchor(&nested), dir.path());
}

#[test]
fn connector_credential_environment_validation_is_bounded_and_secret_scoped() {
    let valid = package::ProviderSetupManifest {
        required_secrets: vec!["echo/api-token".to_string()],
        credential_environment: vec![package::ConnectorCredentialEnvironmentManifest {
            secret: "echo/api-token".to_string(),
            environment_names: vec!["ECHO_API_TOKEN".to_string()],
        }],
        ..package::ProviderSetupManifest::default()
    };
    let issues = package::credential_environment_issues(&valid);
    assert!(issues.is_empty(), "issues={issues:?}");

    let invalid = package::ProviderSetupManifest {
        required_secrets: vec!["echo/api-token".to_string(), "echo/other-token".to_string()],
        credential_environment: vec![
            package::ConnectorCredentialEnvironmentManifest {
                secret: "echo/undeclared".to_string(),
                environment_names: vec!["bad-name".to_string(), "SHARED_TOKEN".to_string()],
            },
            package::ConnectorCredentialEnvironmentManifest {
                secret: "echo/undeclared".to_string(),
                environment_names: vec!["DUPLICATE".to_string(), "DUPLICATE".to_string()],
            },
            package::ConnectorCredentialEnvironmentManifest {
                secret: "echo/other-token".to_string(),
                environment_names: vec!["SHARED_TOKEN".to_string()],
            },
        ],
        ..package::ProviderSetupManifest::default()
    };
    let joined = package::credential_environment_issues(&invalid)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("must also appear in required_secrets"));
    assert!(joined.contains("repeats secret 'echo/undeclared'"));
    assert!(joined.contains("name 'bad-name' must use uppercase"));
    assert!(joined.contains("repeats environment name 'DUPLICATE'"));
    assert!(joined.contains("name 'SHARED_TOKEN' is assigned to both"));
}

#[test]
fn connector_contract_v2_requires_service_metadata() {
    let mut failures = Vec::new();
    validate_service_metadata("echo", None, 1, &mut failures);
    assert!(failures.is_empty(), "v1 remains compatible: {failures:?}");

    validate_service_metadata("echo", None, 2, &mut failures);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("connector contract v2"));
}

#[test]
fn package_gates_share_exact_input_exclusions() {
    let dir = tempfile::tempdir().unwrap();
    for relative in [".harn/workflow-policy", "src/.harn-runs/session"] {
        let internal = dir.path().join(relative);
        fs::create_dir_all(&internal).unwrap();
        fs::write(internal.join("invalid.harn"), "this is not Harn").unwrap();
        fs::write(
            internal.join("guide.md"),
            "```harn\nthis is not Harn\n```\n",
        )
        .unwrap();
        fs::write(internal.join("payload.txt"), "internal").unwrap();
    }
    let generated_docs = dir.path().join("docs/dist");
    fs::create_dir_all(&generated_docs).unwrap();
    fs::write(generated_docs.join("invalid.harn"), "this is not Harn").unwrap();
    fs::write(
        generated_docs.join("guide.md"),
        "```harn\nthis is not Harn\n```\n",
    )
    .unwrap();
    fs::write(generated_docs.join("payload.txt"), "generated").unwrap();
    fs::write(dir.path().join("lib.harn"), "pub fn value() { return 1 }\n").unwrap();
    fs::write(dir.path().join("README.md"), "# Package\n").unwrap();
    fs::write(dir.path().join("payload.txt"), "public").unwrap();

    assert_eq!(package_harn_file_args(dir.path()), ["lib.harn"]);

    let mut markdown = Vec::new();
    collect_markdown_files(dir.path(), dir.path(), &mut markdown);
    let markdown = markdown
        .into_iter()
        .map(|path| path.strip_prefix(dir.path()).unwrap().to_path_buf())
        .collect::<Vec<_>>();
    assert_eq!(markdown, [PathBuf::from("README.md")]);

    assert_eq!(
        package::collect_package_files(dir.path()).unwrap(),
        ["README.md", "lib.harn", "payload.txt"]
    );
}

#[test]
fn package_test_discovery_recurses_through_owned_test_lanes() {
    let dir = tempfile::tempdir().unwrap();
    for relative in [
        "tests/unit/parse.harn",
        "tests/contract/provider.harn",
        "tests/integration/workflow.harn",
    ] {
        let path = dir.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "pipeline test_case(harness: Harness, task) {}\n").unwrap();
    }
    fs::write(
        dir.path().join("tests/unit/helpers.harn"),
        "fn helper() { return true }\n",
    )
    .unwrap();
    for relative in [
        "tests/.harn/generated.harn",
        "tests/.harn-runs/session/generated.harn",
    ] {
        let path = dir.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "pipeline test_generated(harness: Harness, task) {}\n",
        )
        .unwrap();
    }

    let files = package_test_files(dir.path())
        .into_iter()
        .map(|path| path.strip_prefix(dir.path()).unwrap().to_path_buf())
        .collect::<Vec<_>>();

    assert_eq!(
        files,
        [
            PathBuf::from("tests/contract/provider.harn"),
            PathBuf::from("tests/integration/workflow.harn"),
            PathBuf::from("tests/unit/parse.harn"),
        ]
    );
}

#[test]
fn package_gate_stderr_label_matches_gate_status() {
    let mut check = PackageVerifyCheck {
        status: "pass".to_string(),
        ..PackageVerifyCheck::default()
    };
    assert_eq!(gate_stderr_label(&check), "diagnostics");

    check.status = "fail".to_string();
    assert_eq!(gate_stderr_label(&check), "failed output");
}

#[test]
fn package_source_gate_commands_add_only_the_typed_strict_policy() {
    let files = vec!["lib.harn".to_string(), "tests/contract.harn".to_string()];

    assert_eq!(
        PackageSourceGate::Check.command(&files, false),
        ["check", "lib.harn", "tests/contract.harn"]
    );
    assert_eq!(
        PackageSourceGate::Check.command(&files, true),
        [
            "check",
            "--strict",
            "--strict-types",
            "lib.harn",
            "tests/contract.harn"
        ]
    );
    assert_eq!(
        PackageSourceGate::Lint.command(&files, false),
        ["lint", "lib.harn", "tests/contract.harn"]
    );
    assert_eq!(
        PackageSourceGate::Lint.command(&files, true),
        ["lint", "--strict", "lib.harn", "tests/contract.harn"]
    );
}

#[test]
fn package_gate_header_projects_the_requested_strict_policy() {
    let mut report = PackageVerifyReport {
        package: "contract-test".to_string(),
        package_kinds: vec!["package".to_string()],
        strict_requested: true,
        status: "pass".to_string(),
        summary: PackageVerifySummary {
            passed: 2,
            failed: 0,
            skipped: 1,
            warnings: 0,
        },
        checks: Vec::new(),
        connector_contract: None,
    };

    assert_eq!(
        gate_report_header(&report),
        "Package verification pass for contract-test (package, strict_requested=true): 2 passed, 0 failed, 1 skipped."
    );

    report.strict_requested = false;
    assert_eq!(
        gate_report_header(&report),
        "Package verification pass for contract-test (package, strict_requested=false): 2 passed, 0 failed, 1 skipped."
    );
}

#[test]
fn package_dependency_path_canonicalizes_relative_package_dir() {
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::Builder::new()
        .prefix("connector-relative-smoke-")
        .tempdir_in(&cwd)
        .unwrap();
    let relative = dir.path().strip_prefix(&cwd).unwrap();

    let dependency_path = package_dependency_path(relative).unwrap();

    assert_eq!(
        dependency_path,
        dir.path().canonicalize().unwrap().display().to_string()
    );
}

#[test]
fn install_import_smoke_is_inapplicable_without_module_exports() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("harn.toml"),
        "[package]\nname = \"contribution-only\"\nversion = \"0.1.0\"\n\
         [[contributes]]\nkind = \"harn.canon\"\nid = \"example\"\n\
         title = \"Example\"\nmanifest = \"canon-packs.json\"\n",
    )
    .unwrap();
    let check = run_install_import_smoke(dir.path(), true);
    assert!(!check.applicable && !check.reached);
    assert_eq!(check.status, "skipped");
    assert!(check.details[0].contains("no module exports"));
}

#[tokio::test]
async fn connector_check_accepts_valid_fixture_package() {
    let _guard = connector_check_test_guard().await;
    let dir = write_package(
        r#"
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "echo event"
kind = "webhook"
body_json = { id = "evt-1", message = "hello" }
expect_type = "event"
expect_kind = "echo.received"
expect_payload_contains = { id = "evt-1", message = "hello" }
expect_event_count = 1
"#,
        r#"
let active_bindings = []

pub fn provider_id() {
  return "echo"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {
harn_schema_name: "EchoEventPayload",
json_schema: {
  type: "object",
  additionalProperties: true,
},
  }
}

pub fn init(_harness: Harness, _ctx) {}

pub fn activate(harness: Harness, bindings) {
  active_bindings = bindings
  harness.obs.metrics_inc("echo_activate_bindings", len(bindings))
}

pub fn shutdown(harness: Harness) {
  harness.obs.metrics_inc("echo_shutdown")
}

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  const _token = harness.secrets.read("echo/api-token")
  return {
type: "event",
event: {
  kind: "echo.received",
  dedupe_key: "echo:" + body.id,
  payload: body,
},
  }
}

pub fn call(_harness: Harness, method, _args) {
  throw "method_not_found:" + method
}
"#,
    );
    let report = check_connector_package(&check_args(dir.path()))
        .await
        .expect("valid package should pass");
    assert_eq!(report.checked_connectors.len(), 1);
    assert_eq!(report.fixture_count, 1);
    assert_eq!(
        report.checked_connectors[0].payload_schema,
        "EchoEventPayload"
    );
}

#[tokio::test]
async fn connector_check_rejects_payload_schema_name_mismatch() {
    let _guard = connector_check_test_guard().await;
    let dir = write_package(
        "",
        r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() {
  return {
name: "EchoEventPayload",
json_schema: {type: "object"},
  }
}
pub fn normalize_inbound(_harness: Harness, _raw) {
  return {type: "reject", status: 400}
}
"#,
    );
    let error = check_connector_package(&check_args(dir.path()))
        .await
        .unwrap_err();
    assert!(error.contains("payload_schema() must return { harn_schema_name, json_schema? }"));
}

#[tokio::test]
async fn connector_check_rejects_missing_setup_metadata() {
    let _guard = connector_check_test_guard().await;
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("harn.toml"),
        r#"
[package]
name = "contract-test"
version = "0.1.0"

[[providers]]
id = "echo"
connector = { harn = "./lib.harn" }
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("lib.harn"),
        r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(_harness: Harness, _raw) { return {type: "reject", status: 400} }
"#,
    )
    .unwrap();
    let error = check_connector_package(&check_args(dir.path()))
        .await
        .unwrap_err();
    assert!(error.contains("provider 'echo' must declare setup metadata"));
}

#[tokio::test]
async fn connector_check_rejects_legacy_immediate_response_wrapper() {
    let _guard = connector_check_test_guard().await;
    let dir = write_package(
        r#"
[[connector_contract.fixtures]]
provider = "echo"
body_json = { id = "evt-1" }
"#,
        r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(_harness: Harness, _raw) {
  return {
immediate_response: {status: 200, body: "ok"},
event: {
  kind: "echo.received",
  dedupe_key: "echo:evt-1",
  payload: {id: "evt-1"},
},
  }
}
"#,
    );
    let error = check_connector_package(&check_args(dir.path()))
        .await
        .unwrap_err();
    assert!(error.contains("normalize_inbound fixture"));
}

#[tokio::test]
async fn connector_check_reports_static_effect_policy_violations() {
    let _guard = connector_check_test_guard().await;
    let dir = write_package(
        "",
        r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(harness: Harness, _raw) {
  harness.net.get("https://example.invalid")
  return {type: "reject", status: 400}
}
"#,
    );
    let error = check_connector_package(&check_args(dir.path()))
        .await
        .unwrap_err();
    assert!(error.contains("connector-effect-policy"), "{error}");
    assert!(error.contains("harness.net.get"), "{error}");
}

#[tokio::test]
async fn connector_check_can_assert_runtime_policy_denial_fixture() {
    let _guard = connector_check_test_guard().await;
    let dir = write_package(
        r#"
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "indirect file read denied"
body_json = { id = "evt-1" }
expect_error_contains = "violated effect policy"
"#,
        r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }

fn read_indirect() {
  return read_file("ambient.txt")
}

pub fn normalize_inbound(_harness: Harness, raw) {
  const _body = raw.body_json
  read_indirect()
  return {type: "reject", status: 400}
}
"#,
    );
    let report = check_connector_package(&check_args(dir.path()))
        .await
        .expect("expected-error fixture should pass");
    assert_eq!(report.fixture_count, 1);
    assert_eq!(
        report.checked_connectors[0].fixtures[0].result_type,
        "error"
    );
}
