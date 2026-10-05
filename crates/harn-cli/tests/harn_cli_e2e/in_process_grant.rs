//! A provider credential granted `to=in_process` authenticates the run's own
//! model call and reaches no spawned command (harn#8913).
//!
//! One real `harn run` per case against a local provider that answers only
//! when the bearer is the granted credential. The script makes a model call,
//! then runs `env` as a child and reports, by name only, whether the
//! credential's variable reached it. The session-scoped grant is the control:
//! the same child sees the variable there, so the in-process result is the
//! grant's audience and not a child that sees nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::test_util::process::harn_e2e_command;

/// The launcher variable the grant snapshots. The run reads the credential
/// only through the grant's exposure name, never under this one.
const LAUNCHER_VAR: &str = "PROBE_LAUNCHER_PROVIDER_SECRET";
/// The name the provider's `auth_env` reads and the grant exposes.
const EXPOSED_VAR: &str = "PROBE_PROVIDER_KEY";
const CREDENTIAL: &str = "probe-credential-value";

struct Case {
    authorized_calls: usize,
    stdout: String,
    #[cfg(target_os = "linux")]
    raw_child_environment_contains_credential: Option<bool>,
}

async fn run_case(grant_suffix: &'static str) -> Case {
    run_case_with_source(grant_suffix, false).await
}

async fn run_case_with_source(grant_suffix: &'static str, parent_store: bool) -> Case {
    run_case_with_owner(grant_suffix, parent_store, false).await
}

async fn run_case_with_owner(
    grant_suffix: &'static str,
    parent_store: bool,
    contained: bool,
) -> Case {
    let root = tempfile::tempdir().unwrap();
    let authorized = Arc::new(AtomicUsize::new(0));
    let counter = authorized.clone();
    #[cfg(target_os = "linux")]
    let child_pid = Arc::new(std::sync::atomic::AtomicU32::new(0));
    #[cfg(target_os = "linux")]
    let observed_environment = Arc::new(std::sync::Mutex::new(None));
    #[cfg(target_os = "linux")]
    let environment_probe = (child_pid.clone(), observed_environment.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
                let counter = counter.clone();
                #[cfg(target_os = "linux")]
                let environment_probe = environment_probe.clone();
                async move {
                    let authorized = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some(&format!("Bearer {CREDENTIAL}"));
                    if !authorized {
                        return (
                            axum::http::StatusCode::UNAUTHORIZED,
                            [("content-type", "application/json")],
                            r#"{"error":{"message":"unauthorized"}}"#.to_string(),
                        );
                    }
                    #[cfg(target_os = "linux")]
                    if parent_store && !contained {
                        let pid = environment_probe.0.load(Ordering::SeqCst);
                        assert!(pid > 0, "the environment probe must identify the actual CLI child");
                        let bytes = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
                        assert!(bytes.split(|byte| *byte == 0).any(|entry| entry.starts_with(b"PATH=")),
                            "a known non-null child environment read is required");
                        let contains_credential = bytes.windows(CREDENTIAL.len())
                            .any(|window| window == CREDENTIAL.as_bytes());
                        *environment_probe.1.lock().unwrap() = Some(contains_credential);
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let model = body["model"].clone();
                    let reply = if body["stream"] == serde_json::json!(true) {
                        let chunk = serde_json::json!({"id": "c", "object": "chat.completion.chunk", "model": model,
                            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}});
                        (
                            axum::http::StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                        )
                    } else {
                        let message = serde_json::json!({"id": "c", "object": "chat.completion", "model": model,
                            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}});
                        (
                            axum::http::StatusCode::OK,
                            [("content-type", "application/json")],
                            message.to_string(),
                        )
                    };
                    reply
                }
            },
        ),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let providers = root.path().join("providers.toml");
    std::fs::write(
        &providers,
        format!(
            r#"
[providers.probe]
display_name = "Probe"
base_url = "http://{address}/v1"
auth_style = "bearer"
auth_env = "{EXPOSED_VAR}"
chat_endpoint = "/chat/completions"
[models."probe-model"]
name = "Probe model"
provider = "probe"
wire_model = "probe-model"
context_window = 8192
"#
        ),
    )
    .unwrap();
    let script = root.path().join("probe.harn");
    std::fs::write(
        &script,
        format!(
            r#"fn main(harness: Harness) {{
  harness.llm.call("hi", nil, {{provider: "probe", model: "probe-model"}})
  harness.stdio.log("llm_call=completed")
  const child = harness.process.exec("env").stdout
  harness.stdio.log("child_sees_{EXPOSED_VAR}=" + to_string(child.contains("{EXPOSED_VAR}=")))
  harness.stdio.log("child_sees_credential_value=" + to_string(child.contains("{CREDENTIAL}")))
}}
"#
        ),
    )
    .unwrap();
    let path = root.path().to_path_buf();
    let source = if parent_store {
        "secret://fixture/provider".to_string()
    } else {
        format!("env:{LAUNCHER_VAR}")
    };
    let grant = format!("probe={source},expose={EXPOSED_VAR}{grant_suffix}");
    let output = if parent_store {
        use harn_vm::secrets::{
            MemorySecretProvider, ParentSecretHandoff, SecretId, PARENT_SECRET_HANDOFF_OPTION,
        };
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        let id = SecretId::new("fixture", "provider");
        let parent =
            MemorySecretProvider::new("fixture-parent").with_secret(id.clone(), CREDENTIAL);
        let handoff = ParentSecretHandoff::capture(&parent, [id]).await.unwrap();
        let mut command = harn_e2e_command();
        command
            .env_clear()
            .current_dir(&path)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &path)
            .env("USERPROFILE", &path)
            .env("HARN_PROVIDERS_CONFIG", path.join("providers.toml"))
            // A received store must bypass ambient backend construction entirely.
            .env("HARN_SECRET_PROVIDERS", "must-not-be-consulted")
            .args(["run".to_string(), "--grant".into(), grant])
            .arg(path.join("probe.harn"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        if contained {
            #[cfg(all(unix, feature = "hostlib"))]
            {
                tokio::task::spawn_blocking(move || contained_parent_output(command, handoff))
                    .await
                    .unwrap()
            }
            #[cfg(not(all(unix, feature = "hostlib")))]
            panic!("this real guardian fixture requires Unix and hostlib");
        } else {
            let mut bytes = Vec::new();
            handoff.write_to(&mut bytes).unwrap();
            command.arg(format!("--{PARENT_SECRET_HANDOFF_OPTION}"));
            let mut command = tokio::process::Command::from(command);
            command.kill_on_drop(true);
            let mut child = command.spawn().unwrap();
            #[cfg(target_os = "linux")]
            child_pid.store(child.id().unwrap(), Ordering::SeqCst);
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(&bytes).await.unwrap();
            drop(stdin);
            child.wait_with_output().await.unwrap()
        }
    } else {
        tokio::task::spawn_blocking(move || {
            harn_e2e_command()
                .current_dir(&path)
                .env("HARN_PROVIDERS_CONFIG", path.join("providers.toml"))
                .env("HARN_SECRET_PROVIDERS", "env")
                // The model call goes to the local fixture above, never a paid
                // provider; the suite-wide kill switch would refuse it.
                .env_remove("HARN_LLM_CALLS_DISABLED")
                .env(LAUNCHER_VAR, CREDENTIAL)
                .env_remove(EXPOSED_VAR)
                .args(["run", "--grant", &grant])
                .arg(path.join("probe.harn"))
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    };
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(target_os = "linux")]
    let raw_child_environment_contains_credential = *observed_environment.lock().unwrap();
    Case {
        authorized_calls: authorized.load(Ordering::SeqCst),
        stdout,
        #[cfg(target_os = "linux")]
        raw_child_environment_contains_credential,
    }
}

#[cfg(all(unix, feature = "hostlib"))]
fn contained_parent_output(
    command: std::process::Command,
    handoff: harn_vm::secrets::ParentSecretHandoff,
) -> std::process::Output {
    use harn_hostlib::process::{
        spawn_harn_with_parent_secrets, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec,
        WaitOutcome,
    };
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::time::Duration;

    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "in_process_grant::contained_parent_guardian_fixture",
        "--ignored",
        "--nocapture",
    ]);
    let spec = SpawnSpec {
        builtin: "contained_parent_secret_e2e",
        program: command.get_program().to_str().unwrap().to_string(),
        args: command
            .get_args()
            .map(|arg| arg.to_str().unwrap().to_string())
            .collect(),
        cwd: command.get_current_dir().map(std::path::Path::to_path_buf),
        env: command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    (
                        key.to_str().unwrap().to_string(),
                        value.to_str().unwrap().to_string(),
                    )
                })
            })
            .collect(),
        env_remove: Vec::new(),
        env_mode: EnvMode::Replace,
        use_stdin: false,
        configure_process_group: true,
        owner_death: OwnerDeathPolicy::KillContainment,
        output_capture: OutputCapture::Pipe,
    };
    let mut child =
        spawn_harn_with_parent_secrets(spec, handoff, Duration::from_secs(30), &|| false).expect(
            "the real process owner must deliver the handoff without relinquishing containment",
        );
    let mut stdout = child.take_stdout().expect("contained stdout");
    let mut stderr = child.take_stderr().expect("contained stderr");
    let WaitOutcome::Exited(status) = child
        .wait_with_timeout(Some(Duration::from_secs(30)), &|| false)
        .expect("contained wait")
    else {
        panic!("contained parent-secret trial did not terminate");
    };
    let code = status
        .code
        .expect("contained trial exited without a signal");
    let mut output = std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout).unwrap();
    stderr.read_to_end(&mut output.stderr).unwrap();
    output
}

#[cfg(all(unix, feature = "hostlib"))]
#[test]
#[ignore = "private guardian entrypoint exercised by the real containment control"]
fn contained_parent_guardian_fixture() {
    harn_hostlib::process::owner_death::run_guardian_from_pipe().unwrap();
}

#[cfg(all(unix, feature = "hostlib"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_store_authenticates_through_real_owner_death_containment() {
    let case = run_case_with_owner(",to=in_process", true, true).await;
    assert_eq!(case.authorized_calls, 1, "{}", case.stdout);
    assert!(
        case.stdout.contains("llm_call=completed"),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout
            .contains(&format!("child_sees_{EXPOSED_VAR}=false")),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout.contains("child_sees_credential_value=false"),
        "{}",
        case.stdout
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_process_provider_grant_authenticates_llm_call_and_reaches_no_child() {
    let case = run_case(",to=in_process").await;
    assert_eq!(case.authorized_calls, 1, "{}", case.stdout);
    assert!(
        case.stdout.contains("llm_call=completed"),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout
            .contains(&format!("child_sees_{EXPOSED_VAR}=false")),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout.contains("child_sees_credential_value=false"),
        "{}",
        case.stdout
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_provider_grant_reaches_the_child_as_the_control() {
    let case = run_case("").await;
    assert_eq!(case.authorized_calls, 1, "{}", case.stdout);
    assert!(
        case.stdout
            .contains(&format!("child_sees_{EXPOSED_VAR}=true")),
        "{}",
        case.stdout
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_store_authenticates_real_in_process_call_without_child_environment_leak() {
    let case = run_case_with_source(",to=in_process", true).await;
    assert_eq!(case.authorized_calls, 1, "{}", case.stdout);
    assert!(
        case.stdout.contains("llm_call=completed"),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout
            .contains(&format!("child_sees_{EXPOSED_VAR}=false")),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout.contains("child_sees_credential_value=false"),
        "{}",
        case.stdout
    );
    #[cfg(target_os = "linux")]
    assert!(
        case.raw_child_environment_contains_credential == Some(false),
        "the actual CLI process environment must be measured and contain no credential"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_store_session_grant_is_the_positive_child_exposure_control() {
    let case = run_case_with_source("", true).await;
    assert_eq!(case.authorized_calls, 1, "{}", case.stdout);
    assert!(
        case.stdout
            .contains(&format!("child_sees_{EXPOSED_VAR}=true")),
        "{}",
        case.stdout
    );
    assert!(
        case.stdout.contains("child_sees_credential_value=true"),
        "{}",
        case.stdout
    );
    #[cfg(target_os = "linux")]
    assert!(
        case.raw_child_environment_contains_credential == Some(false),
        "an explicit grandchild grant must not put the credential in the CLI environment"
    );
}
