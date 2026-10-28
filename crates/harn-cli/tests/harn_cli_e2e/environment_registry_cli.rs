use crate::test_util::process::run_harn_e2e;

#[test]
fn script_environment_reads_distinguish_refused_from_unset_without_values() {
    use crate::test_util::process::harn_e2e_command;

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("main.harn"),
        r#"
fn main(harness: Harness) {
  harness.stdio.println(harness.env.get("SETTINGS_OVERLAY") == nil)
  harness.stdio.println(harness.env.get_or("SETTINGS_OVERLAY", "default"))
  harness.stdio.println(harness.env.get("LC_MESSAGES") == nil)
  harness.stdio.println(harness.env.get("lc_messages") == nil)
}
"#,
    )
    .unwrap();
    let sentinel = "private-overlay-path-that-must-not-be-logged";
    for (policy, grant, set, expected, refused) in [
        ("granted", None, true, "true\ndefault\ntrue", true),
        ("granted", None, false, "true\ndefault\ntrue", true),
        ("isolated", None, true, "true\ndefault\ntrue", true),
        ("inherited", None, false, "true\ndefault\ntrue", false),
        (
            "granted",
            Some("overlay=env:OVERLAY_SOURCE,expose=SETTINGS_OVERLAY"),
            true,
            "false\nvisible\ntrue",
            false,
        ),
        (
            "granted",
            Some("overlay=env:OVERLAY_SOURCE,expose=SETTINGS_OVERLAY,to=in_process"),
            true,
            "false\nvisible\ntrue",
            false,
        ),
        (
            "granted",
            Some("overlay=env:OVERLAY_SOURCE,expose=SETTINGS_OVERLAY,for=sh"),
            true,
            "true\ndefault\ntrue",
            true,
        ),
    ] {
        let mut command = harn_e2e_command();
        command
            .current_dir(project.path())
            .args(["run", "main.harn", "--environment-policy", policy])
            .env("HARN_LOG_LEVEL", "warn")
            .env("OVERLAY_SOURCE", "visible")
            .env_remove("LC_MESSAGES")
            .env_remove("lc_messages")
            .env_remove("SETTINGS_OVERLAY");
        if set {
            command.env("SETTINGS_OVERLAY", sentinel);
        }
        if let Some(grant) = grant {
            command.args(["--grant", grant]);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{policy}: {stderr}");
        assert_eq!(
            stdout.trim(),
            format!("{expected}\ntrue"),
            "{policy}: {stderr}"
        );
        assert_eq!(
            stderr
                .matches("environment read \"SETTINGS_OVERLAY\" refused")
                .count(),
            if refused { 2 } else { 0 },
            "{policy}: {stderr}",
        );
        assert!(!stderr.contains("LC_MESSAGES"), "{stderr}");
        assert_eq!(
            stderr
                .matches("environment read \"lc_messages\" refused")
                .count(),
            usize::from(policy != "inherited" && !cfg!(windows)),
            "{policy}: {stderr}",
        );
        assert!(!stderr.contains(sentinel), "{stderr}");
        assert!(!stderr.contains("visible"), "{stderr}");
    }
}

#[cfg(unix)]
#[test]
fn script_mcp_retains_the_host_ceiling_without_exposing_it_to_an_ordinary_child() {
    use super::harn_serve_mcp_cli::stable_request;
    use crate::test_util::process::harn_e2e_command;
    use crate::test_util::stdio_jsonrpc::StdioJsonRpcClient;
    use serde_json::json;

    let project = tempfile::tempdir().expect("private MCP child fixture");
    std::fs::write(
        project.path().join("server.harn"),
        r#"
fn main(harness: Harness) {
  const tools = tool_define(tool_registry(), "inspect_child", "Inspect child audience.", {
    parameters: {},
    handler: {args ->
      const child = harness.process.run({program: "/usr/bin/env", args: []})
      json_stringify({
        exit: child.exit_code,
        host_floor_present: harness.env.get("HARN_INFERENCE_BOUNDARY_JSON") != nil,
        floor_withheld: !child.stdout.contains("HARN_INFERENCE_BOUNDARY_JSON="),
        account_hidden: harness.env.get("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON") == nil,
        account_withheld: !child.stdout.contains("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON="),
        sentinel_retained: child.stdout.contains("UNRELATED_SENTINEL=retained"),
      })
    },
  })
  harness.tools.mcp_tools(tools)
}
"#,
    )
    .unwrap();
    let mut command = harn_e2e_command();
    command
        .current_dir(project.path())
        .args(["serve", "mcp", "server.harn", "--surface", "script"])
        .env(
            "HARN_INFERENCE_BOUNDARY_JSON",
            r#"{"reach":"local_only","allow_training_discounts":false}"#,
        )
        .env("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON", "[]")
        .env("UNRELATED_SENTINEL", "retained");
    let mut client = StdioJsonRpcClient::spawn("script MCP child audience", command);
    let response = client.request(stable_request(
        1,
        "tools/call",
        json!({"name": "inspect_child", "arguments": {}}),
    ));
    assert_ne!(response["result"]["isError"], true, "{response}");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("the actual child returned its audience receipt");
    let receipt: serde_json::Value = serde_json::from_str(text).expect("structured child receipt");
    assert_eq!(receipt["exit"], 0, "{receipt}");
    assert_eq!(receipt["host_floor_present"], true, "{receipt}");
    assert_eq!(receipt["floor_withheld"], true, "{receipt}");
    assert_eq!(receipt["account_hidden"], true, "{receipt}");
    assert_eq!(receipt["account_withheld"], true, "{receipt}");
    assert_eq!(receipt["sentinel_retained"], true, "{receipt}");
    client.shutdown_expect_success();
}

#[test]
fn app_rejects_malformed_host_ceiling_before_loading_registry_or_binding() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mcp_server.harn");
    let output = run_harn_e2e(
        &["app", "run", script.to_str().unwrap()],
        &[("HARN_INFERENCE_BOUNDARY_JSON", "{")],
    );
    assert_ne!(output.exit_code, 0);
    assert!(
        output
            .stderr
            .contains("inference_boundary.host_boundary_malformed"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains("serving"), "{}", output.stderr);
}

#[test]
fn worker_rejects_malformed_host_ceiling_before_loading_job_module() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/jobs/scan/scan.harn");
    let output = run_harn_e2e(
        &["serve", "worker", script.to_str().unwrap()],
        &[("HARN_INFERENCE_BOUNDARY_JSON", "{")],
    );
    assert_ne!(output.exit_code, 0);
    assert!(
        output
            .stderr
            .contains("inference_boundary.host_boundary_malformed"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains("worker ready"), "{}", output.stderr);
}

#[test]
fn orchestrator_rejects_malformed_host_ceiling_before_starting_its_runtime_thread() {
    let directory = tempfile::tempdir().expect("private orchestrator fixture");
    let manifest = directory.path().join("harn.toml");
    let state = directory.path().join("state");
    std::fs::write(&manifest, "[package]\nname = \"bootstrap-fixture\"\n").unwrap();
    let output = run_harn_e2e(
        &[
            "orchestrator",
            "serve",
            "--config",
            manifest.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--bind",
            "127.0.0.1:0",
        ],
        &[("HARN_INFERENCE_BOUNDARY_JSON", "{")],
    );
    assert_ne!(output.exit_code, 0);
    assert!(
        output
            .stderr
            .contains("inference_boundary.host_boundary_malformed"),
        "{}",
        output.stderr
    );
    assert!(
        !state.exists(),
        "the runtime created state before validating its ceiling"
    );
}

#[test]
fn script_mcp_rejects_malformed_host_ceiling_before_loading_registry() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mcp_server.harn");
    let output = run_harn_e2e(
        &[
            "serve",
            "mcp",
            script.to_str().unwrap(),
            "--surface",
            "script",
        ],
        &[("HARN_INFERENCE_BOUNDARY_JSON", "{")],
    );
    assert_ne!(output.exit_code, 0);
    assert!(
        output
            .stderr
            .contains("inference_boundary.host_boundary_malformed"),
        "{}",
        output.stderr
    );
    assert!(
        !output.stderr.contains("serving 2 tools"),
        "{}",
        output.stderr
    );
}

#[test]
fn playground_rejects_malformed_host_ceiling_before_provider_bootstrap_or_source_load() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let host = root.join("examples/playground/host.harn");
    let script = root.join("examples/playground/echo.harn");
    let output = run_harn_e2e(
        &[
            "playground",
            "--host",
            host.to_str().unwrap(),
            "--script",
            script.to_str().unwrap(),
        ],
        &[("HARN_INFERENCE_BOUNDARY_JSON", "{")],
    );
    assert_ne!(output.exit_code, 0);
    assert!(
        output
            .stderr
            .contains("inference_boundary.host_boundary_malformed"),
        "{}",
        output.stderr
    );
    assert!(
        !output.stderr.contains("Arity mismatch"),
        "source loaded before bootstrap refusal: {}",
        output.stderr
    );
}

#[test]
fn cli_rejects_unknown_harn_environment_name_before_dispatch() {
    let output = run_harn_e2e(&["--version"], &[("HARN_LLM_TIMOUT", "30")]);

    assert_eq!(output.exit_code, 2);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.contains("HARN-ENV-001"), "{}", output.stderr);
    assert!(
        output.stderr.contains("HARN_LLM_TIMOUT"),
        "{}",
        output.stderr
    );
    assert!(
        output.stderr.contains("HARN_LLM_TIMEOUT"),
        "{}",
        output.stderr
    );
    assert!(
        output
            .stderr
            .contains("Use `HARN_EXT_<NAME>` for settings owned by a calling tool"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains("=30"), "{}", output.stderr);
}

#[test]
fn cli_accepts_caller_owned_environment_name_in_extension_namespace() {
    let output = run_harn_e2e(&["--version"], &[("HARN_EXT_RELEASE_REPO", "/tmp/repo")]);

    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert!(output.stdout.starts_with("harn "), "{}", output.stdout);
    assert!(output.stderr.is_empty(), "{}", output.stderr);
}

#[test]
fn cli_rejects_invalid_registered_value_without_rendering_it() {
    let invalid_value = "not-a-duration";
    let output = run_harn_e2e(&["--version"], &[("HARN_LLM_TIMEOUT", invalid_value)]);

    assert_eq!(output.exit_code, 2);
    assert!(output.stderr.contains("HARN-ENV-002"), "{}", output.stderr);
    assert!(
        output.stderr.contains("HARN_LLM_TIMEOUT"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains(invalid_value), "{}", output.stderr);
}

#[test]
fn playground_subprocess_environment_does_not_require_an_inference_ceiling() {
    use crate::test_util::process::harn_e2e_command;

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("host.harn"),
        "pub fn host_marker() -> string { return \"host-context\" }",
    )
    .unwrap();
    std::fs::write(
        project.path().join("pipeline.harn"),
        r#"
import { command_run } from "std/command"

pipeline default(harness: Harness) {
  assert(harness.env.get("HARN_TASK") == "probe")
  assert(harness.env.get("HARN_LLM_PROVIDER") == "mock")
  assert(harness.env.get("HARN_LLM_MODEL") == "mock")
  const child = command_run(harness.tools, {mode: "argv", argv: ["git", "--version"]})
  assert(child.success && child.stdout.starts_with("git version "))
  harness.stdio.println("${host_marker()}: child ran")
}
"#,
    )
    .unwrap();
    for boundary in [
        None,
        Some(r#"{"reach":"local_only","allow_training_discounts":false}"#),
    ] {
        let mut command = harn_e2e_command();
        command
            .current_dir(project.path())
            .args([
                "playground",
                "--host",
                "host.harn",
                "--script",
                "pipeline.harn",
                "--task",
                "probe",
                "--llm",
                "mock:mock",
            ])
            .env_remove("HARN_INFERENCE_BOUNDARY_JSON");
        if let Some(boundary) = boundary {
            command.env("HARN_INFERENCE_BOUNDARY_JSON", boundary);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "host-context: child ran"
        );
    }
}
