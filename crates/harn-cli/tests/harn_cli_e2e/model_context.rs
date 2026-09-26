use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::test_util::process::harn_e2e_command;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ollama_context_matches_provider_qualified_warmup_settings() {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route(
            "/api/tags",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"models": [
                    {"name": "gpt-4.1-mini"}, {"name": "uncatalogued"}
                ]}))
            }),
        )
        .route(
            "/api/ps",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"models": []})) }),
        )
        .route(
            "/api/show",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({"model_info": {"general.context_length": 131072}}))
            }),
        )
        .route(
            "/api/generate",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                captured.lock().unwrap().push(body);
                async { axum::Json(serde_json::json!({"done": true, "response": ""})) }
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(
        root.path().join("providers.toml"),
        format!(
            r#"
[providers.ollama]
display_name = "Ollama context fixture"
base_url = "http://{address}"
base_url_env = "HARN_TEST_OLLAMA_CONTEXT_HOST"
auth_style = "none"
chat_endpoint = "/api/chat"
completion_endpoint = "/api/generate"
[providers.ollama.local_runtime]
kind = "daemon_api"
wire_protocol = "ollama_api"
command = "ollama"
default_port = 11434
stop = "keep_alive_zero"
[models."context_ollama/model"]
name = "Provider-owned local context"
provider = "ollama"
wire_model = "gpt-4.1-mini"
context_window = 131072
runtime_context_window = 49152
[aliases.context_ollama]
id = "gpt-4.1-mini"
provider = "ollama"
[aliases.context_ollama_unknown]
id = "uncatalogued"
provider = "ollama"
"#
        ),
    )
    .unwrap();
    for (model, configured, expected) in [
        ("context_ollama", Some("4096"), 4096),
        ("context_ollama", None, 49152),
        ("context_ollama_unknown", None, 32768),
    ] {
        let path = root.path().to_path_buf();
        let result = tokio::task::spawn_blocking(move || {
            let mut command = harn_e2e_command();
            command
                .current_dir(&path)
                .env("HARN_HOST_PROVIDERS_CONFIG", path.join("providers.toml"))
                .env("HARN_PROVIDERS_CONFIG", path.join("providers.toml"))
                .env_remove("HARN_OLLAMA_NUM_CTX")
                .env_remove("HARN_TEST_OLLAMA_CONTEXT_HOST")
                .env_remove("OLLAMA_NUM_CTX")
                .env_remove("OLLAMA_CONTEXT_LENGTH")
                .args(["models", "info", "--warm", model]);
            if let Some(value) = configured {
                command.env("HARN_OLLAMA_NUM_CTX", value);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(result["context_window"], expected, "{result}");
        assert_eq!(
            result["readiness"]["expected"]["num_ctx"], expected,
            "{result}"
        );
        let requests = requests.lock().unwrap();
        let request = requests
            .last()
            .expect("warm-up must reach the selected provider");
        assert_eq!(request["options"]["num_ctx"], expected);
        assert_eq!(
            request["model"],
            if model == "context_ollama" {
                "gpt-4.1-mini"
            } else {
                "uncatalogued"
            }
        );
    }
    assert_eq!(requests.lock().unwrap().len(), 3);
    // The script healthcheck must select the declared dialect too, rather than
    // recognizing only a provider literally named `ollama`.
    let config_path = root.path().join("providers.toml");
    let custom_config = std::fs::read_to_string(&config_path)
        .unwrap()
        .replace("providers.ollama", "providers.context_ollama")
        .replace("provider = \"ollama\"", "provider = \"context_ollama\"");
    std::fs::write(&config_path, custom_config).unwrap();
    let script = root.path().join("healthcheck.harn");
    std::fs::write(
        &script,
        r#"fn main(harness: Harness) {
  harness.llm.provider_capabilities_install("[provider_defaults.context_ollama]\nmessage_wire_format = \"ollama\"\n")
  const result = harness.llm.healthcheck("context_ollama", {model: "context_ollama", warm: true})
  assert_eq(result.valid, true)
  assert_eq(result.expected.num_ctx, 49152)
}"#,
    )
    .unwrap();
    let output = tokio::task::spawn_blocking(move || {
        harn_e2e_command()
            .current_dir(script.parent().unwrap())
            .env("HARN_HOST_PROVIDERS_CONFIG", &config_path)
            .env("HARN_PROVIDERS_CONFIG", &config_path)
            .env_remove("HARN_TEST_OLLAMA_CONTEXT_HOST")
            .env_remove("HARN_OLLAMA_NUM_CTX")
            .env_remove("OLLAMA_NUM_CTX")
            .env_remove("OLLAMA_CONTEXT_LENGTH")
            .args(["run", script.to_str().unwrap(), "--no-sandbox"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 4);
    assert_eq!(captured[3]["options"]["num_ctx"], 49152);
    drop(captured);
    server.abort();
}

fn info(root: &std::path::Path, model: &str) -> serde_json::Value {
    let output = harn_e2e_command()
        .current_dir(root)
        .env("HARN_HOST_PROVIDERS_CONFIG", root.join("providers.toml"))
        .args(["models", "info", model])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn model_context_follows_provider_catalog_and_overlay_changes() {
    let root = tempfile::tempdir().unwrap();
    for window in [1_047_576, 65_537] {
        std::fs::write(
            root.path().join("providers.toml"),
            format!(
                r#"
[models."gpt-4.1-mini"]
name = "Context ownership witness"
provider = "openai"
context_window = {window}
"#
            ),
        )
        .unwrap();
        let result = info(root.path(), "gpt-4.1-mini");
        assert_eq!(result["provider"], "openai");
        assert_eq!(result["context_window"], window);
        assert_eq!(result["catalog"]["context_window"], window);
        assert_eq!(result["catalog"]["name"], "Context ownership witness");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_context_discovery_wins_over_advertised_model_family() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async {
                axum::Json(
                    serde_json::json!({"data":[{"id":"gpt-4.1-mini", "max_model_len":4096}]}),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(
        root.path().join("providers.toml"),
        format!(
            r#"
[providers.context_fixture]
display_name = "Context fixture"
base_url = "http://{address}/v1"
auth_style = "bearer"
chat_endpoint = "/chat/completions"
[models."context_fixture/model"]
name = "Local model"
provider = "context_fixture"
wire_model = "gpt-4.1-mini"
context_window = 1000000
runtime_context_window = 32768
[aliases.context_fixture]
id = "gpt-4.1-mini"
provider = "context_fixture"
[aliases.context_unknown]
id = "gpt-4.1-unserved"
provider = "context_fixture"
"#
        ),
    )
    .unwrap();
    let path = root.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || info(&path, "context_fixture"))
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "server discovery must fire"
    );
    assert_eq!(result["provider"], "context_fixture");
    assert_eq!(result["catalog"]["name"], "Local model");
    assert_eq!(result["catalog"]["context_window"], 1000000);
    assert_eq!(result["context_window"], 4096);
    let path = root.path().to_path_buf();
    let unknown = tokio::task::spawn_blocking(move || info(&path, "context_unknown"))
        .await
        .unwrap();
    server.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(unknown["context_window"].is_null(), "{unknown}");
    assert!(unknown["catalog"].is_null(), "{unknown}");
}
