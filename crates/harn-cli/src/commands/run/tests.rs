use super::harnpack::HarnpackRunOptions;
use super::{
    default_run_workspace_root, eval_source_for_code, execute_explain_cost, execute_run,
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
    let code = "const a = 1\nimport \"./lib\"";
    let (header, body) = split_eval_header(code);
    assert_eq!(header, "");
    assert_eq!(body, "const a = 1\nimport \"./lib\"");
}

#[test]
fn eval_source_wraps_pipeline_body_snippets() {
    assert_eq!(
        eval_source_for_code("let x = 1\n__io_println(x)"),
        "pipeline main(task) {\nlet x = 1\n__io_println(x)\n}"
    );
}

#[test]
fn eval_source_keeps_full_harn_programs_unnested() {
    let code = "pipeline default() {\n  __io_println(\"ok\")\n}\n";
    assert_eq!(eval_source_for_code(code), code);
}

#[test]
fn eval_source_keeps_imported_full_harn_programs_unnested() {
    let code = "import { x } from \"./lib\"\n\npipeline default() {\n  __io_println(x)\n}\n";
    assert_eq!(eval_source_for_code(code), code);
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
        read_only_roots: vec!["/tmp/shared".to_string()],
        sandbox_profile: harn_vm::orchestration::SandboxProfile::OsHardened,
        ..harn_vm::orchestration::CapabilityPolicy::default()
    };
    harn_vm::orchestration::push_execution_policy(policy);

    let metadata = run_sandbox_attestation(&RunSandboxOptions::disabled());

    assert_eq!(metadata["run_default_enabled"], false);
    assert_eq!(metadata["active"], true);
    assert_eq!(metadata["workspace_roots"][0], "/tmp/workspace");
    assert_eq!(metadata["write_roots"].as_array().unwrap().len(), 0);
    assert_eq!(metadata["read_only_roots"][0], "/tmp/shared");
    assert_eq!(metadata["profile"], "os_hardened");
    assert_eq!(metadata["egress"], "host_policy");
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_allows_read_from_explicit_read_only_root_but_denies_write() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let read_only_root = temp.path().join("shared");
    std::fs::create_dir(&project).expect("create project");
    std::fs::create_dir(&read_only_root).expect("create read-only root");
    std::fs::write(project.join("harn.toml"), "").expect("write manifest");
    let secret = read_only_root.join("payload.txt");
    let protected = read_only_root.join("prohibited.txt");
    std::fs::write(&secret, "payload").expect("write payload");

    let script = project.join("main.harn");
    let secret_literal = secret.to_string_lossy().replace('\\', "\\\\");
    let prohibited_literal = protected.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        &script,
        format!(
            r#"
pipeline main() {{
  __io_println(read_file("{secret_literal}"))
}}
"#,
        ),
    )
    .expect("write read script");

    let sandbox_options =
        || RunSandboxOptions::default().with_read_only_roots(vec![read_only_root.clone()]);
    let read_outcome = execute_run_with_harnpack_and_sandbox_options(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        sandbox_options(),
        HarnpackRunOptions::default(),
    )
    .await;

    assert_eq!(
        read_outcome.exit_code, 0,
        "stderr:\n{}",
        read_outcome.stderr
    );
    assert_eq!(read_outcome.stdout.trim(), "payload");

    std::fs::write(
        &script,
        format!(
            r#"
pipeline main() {{
  write_file("{prohibited_literal}", "should be denied")
}}
"#,
        ),
    )
    .expect("write denial script");

    let outcome = execute_run_with_harnpack_and_sandbox_options(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        sandbox_options(),
        HarnpackRunOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 1, "stderr:\n{}", outcome.stderr);
    assert!(
        outcome.stderr.contains("under a read-only workspace root"),
        "stderr:\n{}",
        outcome.stderr
    );
    assert!(
        !protected.exists(),
        "write under read-only root must be denied"
    );
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_allows_write_to_explicit_write_root() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let write_root = temp.path().join("external-receipts");
    std::fs::create_dir(&project).expect("create project");
    std::fs::create_dir(&write_root).expect("create write root");
    std::fs::write(project.join("harn.toml"), "").expect("write manifest");

    let target = write_root.join("2026-07-06 Example 5.00.pdf");
    let target_literal = target.to_string_lossy().replace('\\', "\\\\");
    let script = project.join("main.harn");
    std::fs::write(
        &script,
        format!(
            r#"
pipeline main() {{
  write_file("{target_literal}", "%PDF-1.4\n")
  __io_println(read_file("{target_literal}"))
}}
"#,
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
        RunSandboxOptions::default().with_write_roots(vec![write_root.clone()]),
        HarnpackRunOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "%PDF-1.4");
    assert_eq!(
        std::fs::read_to_string(&target).expect("read generated target"),
        "%PDF-1.4\n"
    );
    harn_vm::reset_thread_local_state();
}

#[cfg(all(feature = "hostlib", unix))]
#[tokio::test]
async fn execute_run_allows_command_run_read_from_read_only_root() {
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let read_only_root = temp.path().join("shared");
    std::fs::create_dir(&project).expect("create project");
    std::fs::create_dir(&read_only_root).expect("create read-only root");
    std::fs::write(project.join("harn.toml"), "").expect("write manifest");
    let secret = read_only_root.join("payload.txt");
    std::fs::write(&secret, "payload").expect("write payload");

    let script = project.join("main.harn");
    let secret_literal = secret.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        &script,
        format!(
            r#"
import {{ command_run }} from "std/command"

pipeline main() {{
  const result = command_run(
    {{argv: ["cat", "{secret_literal}"]}},
    {{capture: {{max_inline_bytes: 8}}, timeout_ms: 5000}},
  )
  if !result.success {{
    throw "command_run failed: exit_code=${{result.exit_code}} stderr=${{result.stderr}}"
  }}
  __io_println(result.stdout)
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
        RunSandboxOptions::default().with_read_only_roots(vec![read_only_root.clone()]),
        HarnpackRunOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "payload");
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
  const _ = read_file("{outside_literal}")
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
  const _ = http_get("https://example.com/")
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
  const _ = hostlib_enable("tools:deterministic")
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
  const _ = hostlib_enable("tools:deterministic")
  const result = hostlib_tools_run_command({
argv: ["sh", "-c", "i=0; while [ $i -lt 2000 ]; do printf x; i=$((i+1)); done"],
capture: {max_inline_bytes: 8},
timeout_ms: 5000,
  })
  __io_println(starts_with(result.command_id, "cmd_"))
  __io_println(len(result.stdout))
  __io_println(result.byte_count)
  const window = hostlib_tools_read_command_output({
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

/// End-to-end regression for the dependency-package source-dir leak.
///
/// A project whose entry pipeline renders a top-level `@alias/...` asset, WITH
/// a materialized path-dependency provider connector under
/// `.harn/packages/<dep>/`, is run the way a user runs it: `cd project && harn
/// run main.harn` (a bare filename, so the entry path has an empty parent).
///
/// `harn run` startup loads the dependency's provider-connector contract to
/// build the manifest provider catalog. That load used to leak its own source
/// dir into the caller's resting thread-local, so the entry pipeline's first
/// `render("@promptdir/...")` resolved against the dependency's `harn.toml`
/// (which lacks the alias) and threw `asset alias 'promptdir' is not defined in
/// [asset_roots] of .../.harn/packages/dep-connector/harn.toml`.
///
/// Red before the fix (exit 1 + asset-alias error), green after (exit 0): the
/// entry asset resolves against the PROJECT root even though a `[dependencies]`
/// provider connector is present.
#[tokio::test]
async fn execute_run_entry_asset_alias_resolves_against_project_not_dependency() {
    let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path();

    // Project manifest declares the asset alias the entry render depends on.
    std::fs::write(
        project.join("harn.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[asset_roots]\npromptdir = \"prompts\"\n",
    )
    .expect("write project manifest");

    // The asset the entry pipeline renders.
    std::fs::create_dir_all(project.join("prompts")).expect("prompts dir");
    std::fs::write(
        project.join("prompts").join("greeting.harn.prompt"),
        "hello from the project prompt\n",
    )
    .expect("write prompt");

    // Materialized path-dependency provider connector. Its own manifest does
    // NOT define the `promptdir` alias, so a leaked source dir surfaces as an
    // asset-alias error anchored on this package's harn.toml.
    let dep = project.join(".harn").join("packages").join("dep-connector");
    std::fs::create_dir_all(dep.join("src")).expect("dep src dir");
    std::fs::write(
        dep.join("harn.toml"),
        "[package]\nname = \"dep-connector\"\nversion = \"0.1.0\"\n\n[[providers]]\nid = \"depwebhook\"\nconnector = { harn = \"src/lib.harn\" }\n",
    )
    .expect("write dep manifest");
    std::fs::write(
        dep.join("src").join("lib.harn"),
        "pub fn provider_id() { return \"depwebhook\" }\npub fn kinds() { return [\"webhook\"] }\npub fn payload_schema() { return \"GenericWebhookPayload\" }\n",
    )
    .expect("write connector module");

    // Lockfile so `harn run` startup loads the installed provider connector.
    std::fs::write(
        project.join("harn.lock"),
        "version = 4\n\n[[package]]\nname = \"dep-connector\"\nsource = \"path+.harn/packages/dep-connector\"\n",
    )
    .expect("write lockfile");

    // Entry pipeline at the PROJECT ROOT, rendering a top-level `@alias`.
    std::fs::write(
        project.join("main.harn"),
        "pipeline main() {\n  let _ = render(\"@promptdir/greeting.harn.prompt\", {})\n}\n",
    )
    .expect("write entry");

    // Run it exactly like `cd project && harn run main.harn`: a bare filename
    // whose parent is empty is what left the resting source dir unestablished.
    let original_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(project).expect("chdir into project");
    let outcome = execute_run_with_harnpack_and_sandbox_options(
        "main.harn",
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
    std::env::set_current_dir(&original_cwd).expect("restore cwd");
    harn_vm::reset_thread_local_state();

    assert!(
        !outcome.stderr.contains("is not defined in [asset_roots]"),
        "entry `@promptdir` render leaked onto the dependency's harn.toml; stderr:\n{}",
        outcome.stderr
    );
    assert_eq!(
        outcome.exit_code, 0,
        "entry `@alias` render must resolve against the project root even with a \
         dependency provider connector present; stderr:\n{}",
        outcome.stderr
    );
}
