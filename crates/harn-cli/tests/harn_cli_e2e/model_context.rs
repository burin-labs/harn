use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::test_util::process::harn_e2e_command;

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
