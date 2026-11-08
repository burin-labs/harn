use crate::support;
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::security::session_environment::SessionEnvironment;
use harn_vm::security::{EnvironmentPolicyKind, GrantSourceSpec, GrantSpec};
use harn_vm::tool_annotations::SideEffectLevel;
use harn_vm::value::VmError;
use std::path::Path;

const TOKEN: &str = "protected-gh-fixture-canary-8827";

const LOGIN_PROBE: &str = r#"
fn main(harness: Harness) {
  const result = harness.process.run({program: "gh", args: ["api", "fixture/probe"]})
  assert(result.success)
  const response = json_parse(result.stdout)
  assert(response.token == "[redacted]")
  assert(!response.token_in_env)
  assert(!result.stderr.contains("8827"))
  harness.stdio.println(response.config_dir)
}
"#;

#[derive(Debug)]
enum ProbeFailure {
    Sandbox(harn_vm::process_sandbox::SandboxMechanismUnavailable),
    Runtime(String),
}

impl From<VmError> for ProbeFailure {
    fn from(error: VmError) -> Self {
        match error {
            VmError::SandboxMechanismUnavailable(refusal) => Self::Sandbox(*refusal),
            other => Self::Runtime(other.to_string()),
        }
    }
}

struct GithubFixture {
    _root: tempfile::TempDir,
    path: String,
    config: String,
}

impl GithubFixture {
    fn new() -> Self {
        // A granted executable must be outside the workspace and host temp roots.
        let home = harn_vm::user_dirs::home_dir().expect("real home directory");
        let root = tempfile::Builder::new()
            .prefix("harn-process-grant-")
            .tempdir_in(home)
            .unwrap();
        let bin = root.path().join("bin");
        let config = root.path().join("config");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(&config).unwrap();
        std::fs::create_dir(config.join("gh")).unwrap();
        std::fs::write(config.join("gh/config.yml"), "git_protocol: https\n").unwrap();
        let name = if cfg!(windows) { "gh.exe" } else { "gh" };
        std::fs::copy(support::process_helper(), bin.join(name)).unwrap();
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap()
        .to_string_lossy()
        .into_owned();
        Self {
            _root: root,
            path,
            config: config.to_string_lossy().into_owned(),
        }
    }

    fn run(&self, body: &str, profile: SandboxProfile) -> Result<String, ProbeFailure> {
        let environment = SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![GrantSpec {
                name: "gh_fixture".into(),
                source: GrantSourceSpec::SecretEnv {
                    var: "GH_FIXTURE_CREDENTIAL".into(),
                },
                expose_as_env: Some("GH_TOKEN".into()),
                for_command: Some("gh".into()),
                expose_to: Default::default(),
            }],
            &|name| match name {
                "GH_FIXTURE_CREDENTIAL" => Some(TOKEN.into()),
                "PATH" => Some(self.path.clone()),
                "XDG_CONFIG_HOME" => Some(self.config.clone()),
                _ => std::env::var(name).ok(),
            },
        )
        .unwrap();
        harn_vm::on_vm_stack(|| {
            harn_vm::reset_thread_local_state();
            harn_vm::stdlib::process::set_session_environment(Some(environment));
            let runtime = harn_clock::test_support::current_thread_runtime();
            let result = runtime.block_on(harn_clock::test_support::with_local_tasks(async {
                let chunk = harn_vm::compile_source(body).map_err(ProbeFailure::Runtime)?;
                let mut vm = harn_vm::new_vm();
                harn_runtime::register_vm_stdlib(&mut vm);
                push_execution_policy(CapabilityPolicy {
                    tools: Vec::new(),
                    capabilities: Default::default(),
                    workspace_roots: vec![std::env::current_dir().unwrap().display().to_string()],
                    read_only_roots: vec![self._root.path().join("bin").display().to_string()],
                    side_effect_level: Some(SideEffectLevel::ProcessExec.as_str().to_string()),
                    recursion_limit: None,
                    tool_arg_constraints: Vec::new(),
                    tool_annotations: Default::default(),
                    sandbox_profile: profile,
                    process_sandbox: Default::default(),
                    process_network_proxy: None,
                });
                let result = vm.execute(&chunk).await.map_err(ProbeFailure::from);
                pop_execution_policy();
                result?;
                Ok(vm.output().to_string())
            }));
            harn_vm::stdlib::process::set_session_environment(None);
            harn_vm::reset_thread_local_state();
            result
        })
    }
}

#[test]
fn foreground_run_delivers_only_a_private_login_and_redacts_before_returning() {
    let fixture = GithubFixture::new();
    let output = fixture
        .run(LOGIN_PROBE, SandboxProfile::WorkspacePaths)
        .expect("foreground login delivery");
    assert_private_login_removed(&output);
}

#[test]
fn worktree_foreground_run_delivers_login_or_refuses_the_measured_host_gap() {
    let fixture = GithubFixture::new();
    let result = fixture.run(LOGIN_PROBE, SandboxProfile::Worktree);
    // Qualification requires actual confinement; ordinary hosted coverage must
    // prove the exact measured refusal instead of treating it as a login read.
    match (
        support::sandbox_host::unavailable_default_warn_worktree_sandbox(),
        result,
    ) {
        (Some(expected), Err(ProbeFailure::Sandbox(actual))) => assert_eq!(actual, expected),
        (None, Ok(output)) => assert_private_login_removed(&output),
        (_, Err(ProbeFailure::Runtime(message))) => panic!("unexpected probe error: {message}"),
        (expected, result) => panic!("host expectation {expected:?}, actual result {result:?}"),
    }
}

fn assert_private_login_removed(output: &str) {
    let directory = output.trim();
    assert!(
        !directory.is_empty(),
        "the protected child must report its actual directory"
    );
    assert!(
        !Path::new(directory).exists(),
        "private login must be removed after the child exits"
    );
    assert!(!output.contains(TOKEN));
}

#[test]
fn foreground_run_refuses_credential_printing_with_a_structured_code() {
    let fixture = GithubFixture::new();
    fixture
        .run(
            r#"
fn main(harness: Harness) {
  let refused = false
  try { harness.process.run({program: "gh", args: ["auth", "token"]}) }
  catch (error) {
    refused = error?.code == "environment_policy.command_grant_refused" && error?.program == "gh"
  }
  assert(refused)
}
"#,
            SandboxProfile::Worktree,
        )
        .expect("structured protected-command refusal");
}
