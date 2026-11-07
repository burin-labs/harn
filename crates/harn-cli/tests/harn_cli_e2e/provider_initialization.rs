use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};

use axum::{body::to_bytes, extract::Request, response::Json, Router};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::test_util::process::harn_e2e_command;

#[derive(Debug)]
struct RequestRecord {
    uri: String,
    body: Value,
}

struct ProviderFixture {
    url: String,
    requests: Arc<Mutex<Vec<RequestRecord>>>,
    server: tokio::task::JoinHandle<()>,
}

impl ProviderFixture {
    async fn start() -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let app = Router::new().fallback(move |request: Request| {
            let recorded = recorded.clone();
            async move {
                let uri = request.uri().to_string();
                let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                recorded.lock().unwrap().push(RequestRecord { uri, body });
                Json(json!({
                    "models": [{"name": "qwen3:8b", "model": "qwen3:8b"}],
                    "id": "fixture", "type": "message", "role": "assistant",
                    "model": "fixture", "stop_reason": "end_turn", "status": "completed",
                    "message": {"role": "assistant", "content": "catalog route reached"},
                    "response": "catalog route reached", "done": true,
                    "content": [{"type": "text", "text": "catalog route reached"}],
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "catalog route reached"}, "finish_reason": "stop"}],
                    "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "catalog route reached"}]}],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            requests,
            server,
        }
    }

    fn take_requests(&self) -> Vec<RequestRecord> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

impl Drop for ProviderFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn command(root: &Path, fixture: &ProviderFixture) -> Command {
    let mut command = harn_e2e_command();
    command.env_clear().current_dir(root).stdin(Stdio::null());
    for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let home = root.join("home");
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("APPDATA", home.join("config"))
        .env("HARN_SECRET_PROVIDERS", "env")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env("HTTP_PROXY", &fixture.url)
        .env("http_proxy", &fixture.url)
        .env("NO_PROXY", "")
        .env("no_proxy", "");
    let mut command = Command::from(command);
    command.kill_on_drop(true);
    command
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[tokio::test]
async fn mock_runs_do_not_probe_or_configure_but_explicit_setup_remains_reachable() {
    let fixture = ProviderFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let source = include_str!("../../../../examples/first-hour/agent.harn");
    std::fs::write(root.path().join("agent.harn"), source).unwrap();
    for args in [vec!["run", "agent.harn"], vec!["run", "-e", source]] {
        let output = command(root.path(), &fixture)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Running: Say hello.\ndone\nMock response to 2-word prompt: Say hello.\n"
        );
        assert!(!text(&output).contains("Ollama"), "{}", text(&output));
        assert!(
            fixture.take_requests().is_empty(),
            "mock execution reached a provider"
        );
    }
    assert!(!root.path().join("home/config/harn/providers.toml").exists());
    let output = command(root.path(), &fixture)
        .args([
            "quickstart",
            "--non-interactive",
            "--provider",
            "ollama",
            "--model",
            "qwen3:8b",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let requests = fixture.take_requests();
    assert!(
        requests
            .iter()
            .any(|request| request.uri == "http://127.0.0.1:11434/api/tags"),
        "the explicit setup positive control never reached the same HTTP client: {requests:?}"
    );
    let config =
        std::fs::read_to_string(root.path().join("home/config/harn/providers.toml")).unwrap();
    assert!(config.contains("default_provider = \"ollama\""));
    assert!(text(&output).contains("Configured provider=ollama model=qwen3:8b"));
    let output = command(root.path(), &fixture)
        .env("HARN_LLM_CALLS_DISABLED", "0")
        .args([
            "run",
            "-e",
            r#"fn main(harness: Harness) {
  const response = harness.llm.call("hello", nil, {model: "quickstart", stream: false})
  harness.stdio.println(response.text)
}"#,
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "catalog route reached\n"
    );
    let requests = fixture.take_requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].body["model"], "qwen3:8b", "{requests:?}");
}

#[tokio::test]
async fn an_unconfigured_real_call_reports_explicit_setup_without_probing_local_servers() {
    let fixture = ProviderFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let output = command(root.path(), &fixture)
        .env("HARN_LLM_CALLS_DISABLED", "0")
        .args([
            "run",
            "-e",
            "fn main(harness: Harness) { harness.llm.call(\"hello\") }",
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("harn quickstart"),
        "{}",
        text(&output)
    );
    assert!(
        text(&output).contains("Missing API key"),
        "{}",
        text(&output)
    );
    assert!(fixture.take_requests().is_empty());
    assert!(!root.path().join("home/config/harn/providers.toml").exists());
}

#[tokio::test]
async fn automatic_calls_use_catalog_credential_order_and_the_complete_route() {
    let fixture = ProviderFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let catalog = harn_provider_catalog::config::packaged_config();
    let routes = harn_provider_catalog::automatic_defaults(&catalog.provider_defaults);
    assert!(
        routes.len() >= 2,
        "the credential-order proof needs two distinct routes"
    );
    let overlay = root.path().join("providers.toml");
    let mut definitions = String::new();
    for route in &routes {
        definitions.push_str(&format!(
            "[providers.{}]\nbase_url = {:?}\n",
            route.provider, fixture.url
        ));
    }
    for (first_key, reverse_order, expected) in [
        (true, false, &routes[0]),
        (false, false, &routes[1]),
        (true, true, &routes[1]),
    ] {
        let mut config = definitions.clone();
        if reverse_order {
            config.push_str(&format!("[provider_defaults.{}]\nautomatic_order = 0\nautomatic = {:?}\nruntime = \"unused-runtime-default\"\n", routes[1].provider, routes[1].model_id));
        }
        std::fs::write(&overlay, config).unwrap();
        let mut child = command(root.path(), &fixture);
        child
            .env("HARN_LLM_CALLS_DISABLED", "0")
            .env("HARN_PROVIDERS_CONFIG", &overlay);
        for (index, route) in routes.iter().enumerate() {
            if index == 0 && !first_key {
                continue;
            }
            let provider = &catalog.providers[&route.provider];
            let name = harn_vm::llm_config::auth_env_names(&provider.auth_env)[0].clone();
            child.env(name, "synthetic-fixture-credential");
        }
        let output = child
            .args([
                "run",
                "-e",
                r#"fn main(harness: Harness) {
  const response = harness.llm.call("hello", nil, {stream: false})
  harness.stdio.println(response.text)
}"#,
            ])
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "catalog route reached\n"
        );
        let requests = fixture.take_requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        let wire_model = catalog.models[&expected.model_id]
            .wire_model
            .as_deref()
            .unwrap_or(&expected.model_id);
        assert_eq!(requests[0].body["model"], wire_model, "{requests:?}");
    }
}
