use super::harnpack::HarnpackRunOptions;
use super::{
    default_run_workspace_root, execute_explain_cost, execute_run,
    execute_run_with_harnpack_and_sandbox_options, run_sandbox_attestation, split_eval_header,
    CliLlmMockMode, RunProfileOptions, RunSandboxOptions, StdoutPassthroughGuard,
};
use std::collections::HashSet;
use std::path::Path;

#[test]
fn split_eval_header_no_imports_returns_full_body() {
    let (header, body) = split_eval_header("log(1 + 2)");
    assert_eq!(header, "");
    assert_eq!(body, "log(1 + 2)");
}

#[test]
fn split_eval_header_lifts_leading_imports() {
    let code = "import \"./lib\"\nimport { x } from \"std/math\"\nlog(x)";
    let (header, body) = split_eval_header(code);
    assert_eq!(header, "import \"./lib\"\nimport { x } from \"std/math\"");
    assert_eq!(body, "log(x)");
}

#[test]
fn split_eval_header_keeps_pub_import_and_comments_in_header() {
    let code = "// header comment\npub import { y } from \"./lib\"\n\nfoo()";
    let (header, body) = split_eval_header(code);
    assert_eq!(
        header,
        "// header comment\npub import { y } from \"./lib\"\n"
    );
    assert_eq!(body, "foo()");
}

#[test]
fn split_eval_header_does_not_lift_imports_after_other_statements() {
    let code = "let a = 1\nimport \"./lib\"";
    let (header, body) = split_eval_header(code);
    assert_eq!(header, "");
    assert_eq!(body, "let a = 1\nimport \"./lib\"");
}

#[test]
fn cli_llm_mock_roundtrips_logprobs() {
    let mock = harn_vm::llm::parse_llm_mock_value(&serde_json::json!({
        "text": "visible",
        "logprobs": [{"token": "visible", "logprob": 0.0}]
    }))
    .expect("parse mock");
    assert_eq!(mock.logprobs.len(), 1);

    let line = harn_vm::llm::serialize_llm_mock(mock).expect("serialize mock");
    let value: serde_json::Value = serde_json::from_str(&line).expect("json line");
    assert_eq!(value["logprobs"][0]["token"].as_str(), Some("visible"));

    let reparsed = harn_vm::llm::parse_llm_mock_value(&value).expect("reparse mock");
    assert_eq!(reparsed.logprobs.len(), 1);
    assert_eq!(reparsed.logprobs[0]["logprob"].as_f64(), Some(0.0));
}

#[test]
fn stdout_passthrough_guard_restores_previous_state() {
    let original = harn_vm::set_stdout_passthrough(false);
    {
        let _guard = StdoutPassthroughGuard::enable();
        assert!(harn_vm::set_stdout_passthrough(true));
    }
    assert!(!harn_vm::set_stdout_passthrough(original));
}

#[test]
fn execute_explain_cost_does_not_execute_script() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
pipeline main() {
  write_file("executed.txt", "bad")
  llm_call("hello", nil, {provider: "mock", model: "mock"})
}
"#,
    )
    .expect("write script");

    let outcome = execute_explain_cost(&script.to_string_lossy());

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert!(outcome.stdout.contains("LLM cost estimate"));
    assert!(
        !temp.path().join("executed.txt").exists(),
        "--explain-cost must not execute pipeline side effects"
    );
}

#[test]
fn default_run_workspace_root_prefers_manifest_root_then_cwd() {
    let project = tempfile::TempDir::new().expect("project");
    let source_parent = project.path().join("scripts");
    let cwd = std::env::current_dir().expect("cwd");

    assert_eq!(
        default_run_workspace_root(Some(project.path()), &source_parent),
        project.path()
    );
    assert_eq!(default_run_workspace_root(None, Path::new("scripts")), cwd);
}

#[test]
fn run_sandbox_attestation_reports_effective_policy() {
    harn_vm::reset_thread_local_state();
    let policy = harn_vm::orchestration::CapabilityPolicy {
        workspace_roots: vec!["/tmp/workspace".to_string()],
        sandbox_profile: harn_vm::orchestration::SandboxProfile::OsHardened,
        ..harn_vm::orchestration::CapabilityPolicy::default()
    };
    harn_vm::orchestration::push_execution_policy(policy);

    let metadata = run_sandbox_attestation(&RunSandboxOptions::disabled());

    assert_eq!(metadata["run_default_enabled"], false);
    assert_eq!(metadata["active"], true);
    assert_eq!(metadata["workspace_roots"][0], "/tmp/workspace");
    assert_eq!(metadata["profile"], "os_hardened");
    assert_eq!(metadata["egress"], "host_policy");
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_default_sandbox_reports_worktree_profile() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r"
pipeline main() {
  __io_println(sandbox_active_profile())
}
",
    )
    .expect("write script");

    let outcome = execute_run(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "worktree");
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_default_sandbox_blocks_outside_workspace_read() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let outside = temp.path().join("outside.txt");
    std::fs::create_dir(&project).expect("create project");
    std::fs::write(project.join("harn.toml"), "").expect("write manifest");
    std::fs::write(&outside, "secret").expect("write outside");
    let script = project.join("main.harn");
    let outside_literal = outside.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        &script,
        format!(
            r#"
pipeline main() {{
  __io_println(sandbox_active_profile())
  let _ = read_file("{outside_literal}")
}}
"#
        ),
    )
    .expect("write script");

    let outcome = execute_run(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 1, "stdout:\n{}", outcome.stdout);
    assert!(
        outcome.stderr.contains("sandbox violation"),
        "stderr:\n{}",
        outcome.stderr
    );
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_no_sandbox_allows_outside_workspace_read() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let outside = temp.path().join("outside.txt");
    std::fs::create_dir(&project).expect("create project");
    std::fs::write(&outside, "secret").expect("write outside");
    let script = project.join("main.harn");
    let outside_literal = outside.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        &script,
        format!(
            r#"
pipeline main() {{
  __io_println(sandbox_active_profile())
  __io_println(read_file("{outside_literal}"))
}}
"#
        ),
    )
    .expect("write script");

    let outcome = execute_run_with_harnpack_and_sandbox_options(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunSandboxOptions::disabled(),
        HarnpackRunOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "unrestricted\nsecret");
    assert!(outcome.stderr.contains("--no-sandbox"));
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_denies_network_by_default() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
pipeline main() {
  let _ = http_get("https://example.com/")
}
"#,
    )
    .expect("write script");

    let outcome = execute_run(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 1, "stdout:\n{}", outcome.stdout);
    assert!(
        outcome.stderr.contains("exceeds network ceiling"),
        "stderr:\n{}",
        outcome.stderr
    );
    harn_vm::reset_thread_local_state();
}

#[cfg(feature = "hostlib")]
#[tokio::test]
async fn execute_run_installs_hostlib_gate() {
    let temp = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(
        temp.path(),
        r#"
pipeline main() {
  let _ = hostlib_enable("tools:deterministic")
  __io_println("enabled")
}
"#,
    )
    .expect("write script");

    let outcome = execute_run(
        &temp.path().to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "enabled");
}

#[cfg(all(feature = "hostlib", unix))]
#[tokio::test]
async fn execute_run_can_read_hostlib_command_artifacts() {
    let temp = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(
        temp.path(),
        r#"
pipeline main() {
  let _ = hostlib_enable("tools:deterministic")
  let result = hostlib_tools_run_command({
argv: ["sh", "-c", "i=0; while [ $i -lt 2000 ]; do printf x; i=$((i+1)); done"],
capture: {max_inline_bytes: 8},
timeout_ms: 5000,
  })
  __io_println(starts_with(result.command_id, "cmd_"))
  __io_println(len(result.stdout))
  __io_println(result.byte_count)
  let window = hostlib_tools_read_command_output({
command_id: result.command_id,
offset: 1990,
length: 20,
  })
  __io_println(len(window.content))
  __io_println(window.eof)
}
"#,
    )
    .expect("write script");

    let outcome = execute_run_with_harnpack_and_sandbox_options(
        &temp.path().to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunSandboxOptions::disabled(),
        HarnpackRunOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "true\n8\n2000\n10\ntrue");
}
