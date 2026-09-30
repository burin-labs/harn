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
}

async fn run_case(grant_suffix: &'static str) -> Case {
    let root = tempfile::tempdir().unwrap();
    let authorized = Arc::new(AtomicUsize::new(0));
    let counter = authorized.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
                let counter = counter.clone();
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
    let grant = format!("probe=env:{LAUNCHER_VAR},expose={EXPOSED_VAR}{grant_suffix}");
    let output = tokio::task::spawn_blocking(move || {
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
    .unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Case {
        authorized_calls: authorized.load(Ordering::SeqCst),
        stdout,
    }
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
