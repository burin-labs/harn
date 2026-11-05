//! How `harn run --grant` hands a launcher value to a child, and when the
//! child's output has it redacted.

use super::super::{EnvironmentPolicyConfig, RunOutcome};
use super::command_grant_diagnostic::COMMAND_GRANT_FAILURE_DIAGNOSTIC;
use super::{
    execute_run_with_harnpack_and_sandbox_options, CliLlmMockMode, HarnpackRunOptions,
    RunProfileOptions, RunSandboxOptions,
};
use std::collections::HashSet;

/// Run `body` as a project script under the one `--grant` spec. `None` when
/// this host cannot start the sandboxed child.
async fn run_granted(body: &str, grant: String) -> Option<RunOutcome> {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("create project");
    std::fs::write(project.join("harn.toml"), "").expect("write manifest");
    let script = project.join("main.harn");
    std::fs::write(&script, [COMMAND_GRANT_FAILURE_DIAGNOSTIC, body].concat())
        .expect("write script");
    let config = EnvironmentPolicyConfig::from_flags(None, &[grant]).expect("valid grant");
    let outcome = execute_run_with_harnpack_and_sandbox_options(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunSandboxOptions::default().with_environment_policy(config),
        HarnpackRunOptions::default(),
    )
    .await;
    #[cfg(target_os = "macos")]
    if crate::tests::common::sandbox_host::assert_unavailable_worktree_refusal(&outcome) {
        return None;
    }
    Some(outcome)
}

/// A command-bound grant injects its snapshotted value into the matching child.
/// The same policy withholds it from a neighboring executable. The parent
/// holds SRC but not CHILD, so the matching child's value proves injection.
#[tokio::test]
async fn execute_run_granted_policy_injects_into_subprocess_env() {
    harn_vm::reset_thread_local_state();
    let src = "HARN_TEST_LANE_GRANT_SRC";
    // SAFETY: this test process mutates its own env; the vars are unique to it
    // and removed before returning.
    std::env::set_var(src, "granted-secret-value");
    std::env::remove_var("HARN_TEST_CHILD_VAR");

    let outcome = run_granted(
        r#"
import { command_run } from "std/command"

pipeline main(harness: Harness) {
  const result = command_run(
    harness.tools,
    {argv: ["sh", "-c", "[ \"$HARN_TEST_CHILD_VAR\" = granted-secret-value ] && printf delivered; printf ' %s' \"$HARN_TEST_CHILD_VAR\""]},
    {capture: {max_inline_bytes: 64}},
  )
  if !result.success {
    throw "command_run failed: ${json_stringify(command_grant_failure(result))}"
  }
  harness.stdio.println(result.stdout)
  const neighbor = command_run(
    harness.tools,
    {argv: ["env"]},
    {capture: {max_inline_bytes: 65536}},
  )
  if !neighbor.success {
    throw "neighbor command failed: ${json_stringify(command_grant_failure(neighbor))}"
  }
  harness.stdio.println(neighbor.stdout.contains("HARN_TEST_CHILD_VAR=") ? "leaked" : "absent")
}
"#,
        format!("grant_source=env:{src},expose=HARN_TEST_CHILD_VAR,for=sh"),
    )
    .await;
    std::env::remove_var(src);

    if let Some(outcome) = outcome {
        assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
        assert_eq!(
            outcome.stdout.lines().collect::<Vec<_>>(),
            ["delivered [redacted]", "absent"]
        );
        assert!(
            outcome.stderr.contains("environment policy: granted"),
            "the grant must be disclosed; stderr:\n{}",
            outcome.stderr
        );
    }
    harn_vm::reset_thread_local_state();
}

/// A session-wide `secret-env:` grant declares its value a credential, so a
/// plain `harn run`, which has no agent session, still redacts it from what a
/// child prints.
#[tokio::test]
async fn execute_run_secret_env_grant_is_redacted_without_a_session() {
    harn_vm::reset_thread_local_state();
    let src = "SECRET_ENV_GRANT_PROBE_SRC";
    // SAFETY: this test process mutates its own env; the vars are unique to it
    // and removed before returning.
    std::env::set_var(src, "declared-secret-env-value");
    std::env::remove_var("SECRET_ENV_GRANT_PROBE_CHILD");

    let outcome = run_granted(
        r#"
import { command_run } from "std/command"

pipeline main(harness: Harness) {
  const result = command_run(
    harness.tools,
    {argv: ["sh", "-c", "[ \"$SECRET_ENV_GRANT_PROBE_CHILD\" = declared-secret-env-value ] && printf delivered; printf ' %s' \"$SECRET_ENV_GRANT_PROBE_CHILD\""]},
    {capture: {max_inline_bytes: 64}},
  )
  if !result.success {
    throw "command_run failed: ${json_stringify(command_grant_failure(result))}"
  }
  harness.stdio.println(result.stdout)
}
"#,
        format!("secret_grant=secret-env:{src},expose=SECRET_ENV_GRANT_PROBE_CHILD"),
    )
    .await;
    std::env::remove_var(src);

    if let Some(outcome) = outcome {
        assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
        assert_eq!(
            outcome.stdout.lines().collect::<Vec<_>>(),
            ["delivered [redacted]"]
        );
        assert!(
            !outcome.stdout.contains("declared-secret-env-value"),
            "a declared credential was printed: {}",
            outcome.stdout
        );
    }
    harn_vm::reset_thread_local_state();
}
