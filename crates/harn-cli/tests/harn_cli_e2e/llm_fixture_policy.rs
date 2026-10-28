//! Real CLI fixture admission against a credential-free loopback SSE provider.

use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

use crate::test_util::process::harn_e2e_command;

const REPLY: &str = "cli-fixture-proof";

#[tokio::test]
async fn project_manifest_cannot_forge_host_privacy_fact_for_live_call() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Provider::start(temp.path(), false).await;
    let host_config = std::fs::read_to_string(&provider.config).unwrap()
        + r#"
[providers.runner-proof.data_controls]
control_scope = "none"
retention_default = "unspecified"
training_default = "unspecified"
checked_on = "2026-10-27"
sources = ["https://example.invalid/host-policy"]
"#;
    std::fs::write(&provider.config, host_config).unwrap();
    std::fs::write(
        temp.path().join("harn.toml"),
        r#"
[llm.providers.runner-proof.data_controls]
control_scope = "none"
retention_default = "unspecified"
training_default = "does_not_train"
checked_on = "2026-10-27"
sources = ["https://example.invalid/project-claim"]
"#,
    )
    .unwrap();
    std::fs::write(temp.path().join("main.harn"), r#"
fn main(harness: Harness) {
  const result = harness.llm.call("fixture", nil, {provider: "runner-proof", model: "runner-proof", stream: true, max_tokens: 16})
  harness.stdio.log(result.text)
}
"#).unwrap();
    let run = |bounded: bool| {
        let mut command = harn_e2e_command();
        command
            .current_dir(temp.path())
            .args(["run", "main.harn"])
            .env("HARN_PROVIDERS_CONFIG", &provider.config)
            .env("HARN_LLM_CALLS_DISABLED", "0")
            .env("HARN_LLM_RATE_LIMIT_DURABLE", "0")
            .env("NO_COLOR", "1")
            .env_remove("HARN_INFERENCE_BOUNDARY_JSON");
        if bounded {
            command.env(
                "HARN_INFERENCE_BOUNDARY_JSON",
                r#"{"reach":"any_hosted","allow_training_discounts":false}"#,
            );
        }
        command
    };
    let mut allowed_command = run(false);
    let mut refused_command = run(true);
    let allowed = tokio::task::spawn_blocking(move || allowed_command.output().unwrap())
        .await
        .unwrap();
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(String::from_utf8_lossy(&allowed.stdout).contains(REPLY));
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "unbounded positive control reached the listener"
    );
    let refused = tokio::task::spawn_blocking(move || refused_command.output().unwrap())
        .await
        .unwrap();
    assert!(
        !refused.status.success(),
        "host ceiling must refuse project privacy claims"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("inference_boundary.training_unknown"),
        "{stderr}"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "bounded call sent no request"
    );
}

struct Provider {
    config: std::path::PathBuf,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Provider {
    async fn start(root: &Path, parallel: bool) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let cases_reached = Arc::new(tokio::sync::Barrier::new(2));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let recorded = recorded.clone();
                let cases_reached = cases_reached.clone();
                async move {
                    let case_call = matches!(body["messages"][0]["content"].as_str(), Some("first" | "second"));
                    recorded.lock().unwrap().push(body);
                    if parallel && case_call {
                        // Each worker must reach HTTP before either case can finish.
                        cases_reached.wait().await;
                    }
                    let chunk = serde_json::json!({
                        "model": "runner-wire-proof",
                        "choices": [{"index": 0, "delta": {"content": REPLY}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 1}
                    });
                    ([ ("content-type", "text/event-stream") ], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = root.join("providers.toml");
        std::fs::write(
            &config,
            format!(
                r#"[providers.runner-proof]
base_url = "http://{address}/v1"
chat_endpoint = "/chat/completions"
auth_style = "none"
auth_env = []
[models."runner-proof"]
name = "Runner proof"
provider = "runner-proof"
wire_model = "runner-wire-proof"
context_window = 8192
[provider_defaults.runner-proof]
runtime = "runner-proof"
"#
            ),
        )
        .unwrap();
        Self {
            config,
            requests,
            server,
        }
    }

    fn assert_recorded(&self) {
        let requests = self.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            3,
            "one file fixture and two case calls must reach HTTP"
        );
        let mut prompts = Vec::new();
        for request in requests.iter() {
            assert_eq!(request["model"], "runner-wire-proof");
            assert_eq!(
                request["stream"], true,
                "the recording must reach SSE transport"
            );
            assert_eq!(request["messages"][0]["role"], "user");
            prompts.push(request["messages"][0]["content"].as_str().unwrap());
        }
        prompts.sort_unstable();
        assert_eq!(prompts, ["first", "fixture", "second"]);
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn invoke(
    root: &Path,
    provider: &Provider,
    mode: Option<&str>,
    parallel: bool,
    selection: &str,
    disabled: bool,
) -> Output {
    let mut command = harn_e2e_command();
    command.current_dir(root).arg("test");
    if let Some(mode) = mode {
        command.arg(mode);
    }
    if parallel {
        command.args(["--parallel", "--jobs", "2"]);
    }
    command
        .args(["tests.harn", "--verbose"])
        .env("HARN_PROVIDERS_CONFIG", &provider.config)
        .env("HARN_LLM_CALLS_DISABLED", if disabled { "1" } else { "0" })
        .env("HARN_LLM_RATE_LIMIT_DURABLE", "0")
        .env("NO_COLOR", "1");
    for name in [
        "HARN_LLM_PROVIDER",
        "HARN_LLM_MODEL",
        "LOCAL_LLM_BASE_URL",
        "LOCAL_LLM_MODEL",
        "HARN_LLM_MOCK_REPLAY",
        "HARN_LLM_MOCK_RECORD",
    ] {
        command.env_remove(name);
    }
    if selection == "environment" {
        command.env("HARN_LLM_MODEL", "runner-proof");
    }
    if selection == "provider" {
        command.env("HARN_LLM_PROVIDER", "runner-proof");
    }
    if selection == "mock" {
        command.env("HARN_LLM_PROVIDER", "mock");
    }
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

fn source(selection: &str) -> String {
    let options = match selection {
        "explicit" => "provider: \"runner-proof\", model: \"runner-proof\",",
        "model" => "model: \"runner-proof\",",
        "environment" | "provider" => "",
        _ => unreachable!(),
    };
    format!(
        r#"const OPTIONS: LlmCallOptions = {{{options} stream: true, max_tokens: 16}}
@test_fixture(scope: file)
fn shared_fixture(harness: Harness) -> dict {{
  const result = harness.llm.call("fixture", nil, OPTIONS)
  assert_eq(result.text, "{REPLY}")
  assert_eq(result.provider, "runner-proof")
  return {{text: result.text}}
}}
@test(fixture: shared_fixture)
pipeline test_record_first(harness: Harness, fixture: dict) {{
  assert_eq(fixture.text, "{REPLY}")
  const result = harness.llm.call("first", nil, OPTIONS)
  assert_eq(result.text, "{REPLY}")
  assert_eq(result.provider, "runner-proof")
}}
pipeline test_record_second(harness: Harness) {{
  const result = harness.llm.call("second", nil, OPTIONS)
  assert_eq(result.text, "{REPLY}")
  assert_eq(result.provider, "runner-proof")
}}
"#
    )
}

fn assert_passed(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("2 passed"),
        "both test bodies must finish: {stdout}\n{stderr}"
    );
}

async fn record_and_replay(parallel: bool) {
    for selection in ["explicit", "model", "environment", "provider"] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::start(root.path(), parallel).await;
        std::fs::write(root.path().join("tests.harn"), source(selection)).unwrap();
        assert_passed(
            &invoke(
                root.path(),
                &provider,
                Some("--record"),
                parallel,
                selection,
                false,
            )
            .await,
        );
        provider.assert_recorded();
        let fixtures = std::fs::read_dir(root.path().join(".harn-fixtures"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(fixtures.len(), 3);
        for fixture in fixtures {
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
            assert_eq!(
                result["text"], REPLY,
                "fixture must contain actual provider output"
            );
            assert_eq!(result["provider"], "runner-proof");
        }
        assert_passed(
            &invoke(
                root.path(),
                &provider,
                Some("--replay"),
                parallel,
                selection,
                true,
            )
            .await,
        );
        provider.assert_recorded(); // Replay under the launcher kill switch never redispatches.
    }
}

#[tokio::test]
async fn serial_record_replay_reaches_file_fixture_and_cases() {
    record_and_replay(false).await;
}

#[tokio::test]
async fn parallel_record_replay_reaches_file_fixture_and_cases() {
    record_and_replay(true).await;
}

#[tokio::test]
async fn record_respects_launcher_kill_and_missing_replay_never_dispatches() {
    for parallel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::start(root.path(), parallel).await;
        std::fs::write(root.path().join("tests.harn"), source("explicit")).unwrap();
        let record = invoke(
            root.path(),
            &provider,
            Some("--record"),
            parallel,
            "explicit",
            true,
        )
        .await;
        assert!(!record.status.success());
        assert!(
            String::from_utf8_lossy(&record.stdout).contains("LLM_CALLS_DISABLED")
                || String::from_utf8_lossy(&record.stderr).contains("LLM_CALLS_DISABLED")
        );
        assert!(provider.requests.lock().unwrap().is_empty());
        assert!(!root.path().join(".harn-fixtures").exists());
        let replay = invoke(
            root.path(),
            &provider,
            Some("--replay"),
            parallel,
            "explicit",
            true,
        )
        .await;
        assert!(!replay.status.success());
        assert!(
            String::from_utf8_lossy(&replay.stdout).contains("No fixture found for LLM call")
                || String::from_utf8_lossy(&replay.stderr)
                    .contains("No fixture found for LLM call")
        );
        assert!(provider.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn ordinary_tests_default_to_mock_and_preserve_explicit_provider() {
    for parallel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::start(root.path(), parallel).await;
        std::fs::write(
            root.path().join("tests.harn"),
            r#"
@test_fixture(scope: file)
fn shared_fixture(harness: Harness) -> dict {
  const result = harness.llm.call("fixture", nil, {})
  assert_eq(result.provider, "mock")
  return {provider: result.provider}
}
@test(fixture: shared_fixture)
pipeline test_mock_first(harness: Harness, fixture: dict) {
  assert_eq(fixture.provider, "mock")
  assert_eq(harness.llm.call("first", nil, {}).provider, "mock")
}
pipeline test_mock_second(harness: Harness) {
  assert_eq(harness.llm.call("second", nil, {}).provider, "mock")
}
"#,
        )
        .unwrap();
        assert_passed(&invoke(root.path(), &provider, None, parallel, "explicit", false).await);
        assert_passed(&invoke(root.path(), &provider, None, parallel, "mock", false).await);
        assert_passed(
            &invoke(
                root.path(),
                &provider,
                Some("--record"),
                parallel,
                "mock",
                false,
            )
            .await,
        );
        assert!(provider.requests.lock().unwrap().is_empty());
        assert!(!root.path().join(".harn-fixtures").exists());
        std::fs::write(root.path().join("tests.harn"), source("provider")).unwrap();
        let selected = invoke(root.path(), &provider, None, parallel, "provider", false).await;
        assert!(!selected.status.success());
        assert!(
            String::from_utf8_lossy(&selected.stdout).contains("LLM_CALLS_DISABLED")
                || String::from_utf8_lossy(&selected.stderr).contains("LLM_CALLS_DISABLED"),
            "explicit provider must be preserved while live test calls remain disabled"
        );
        assert!(provider.requests.lock().unwrap().is_empty());
    }
}
