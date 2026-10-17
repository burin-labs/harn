//! The documented allocation example must reach real HTTP, including its refusal control.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};

async fn run(root: &Path, config: &Path, args: &[&str]) -> serde_json::Value {
    let mut command = harn_e2e_command();
    command
        .current_dir(root)
        .args([
            "run",
            "--standalone",
            "--no-sandbox",
            "examples/conservative-experiment.harn",
            "--",
        ])
        .args(args)
        .env("HARN_BIN", harn_e2e_binary())
        .env("HARN_PROVIDERS_CONFIG", config)
        .env("OPENAI_API_KEY", "conservative-experiment-fixture")
        .env("HARN_LLM_CALLS_DISABLED", "0")
        .env("HARN_LLM_RATE_LIMIT_DURABLE", "0")
        .env_remove("HARN_LLM_PROVIDER")
        .env_remove("HARN_LLM_MODEL")
        .env_remove("HARN_LLM_MOCK_REPLAY")
        .env_remove("HARN_LLM_MOCK_RECORD")
        .env_remove("HARN_SPEND_POLICY");
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("example report")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_and_grader_allocations_reach_transport_and_refuse_an_unaffordable_call() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("examples")).unwrap();
    std::fs::write(
        temp.path().join("examples/conservative-experiment.harn"),
        include_str!("../../../../examples/conservative-experiment.harn"),
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            recorded.lock().unwrap().push(body.clone());
            async move {
                let prompt = body["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap();
                let text = if prompt.starts_with("Does this Python") {
                    "PASS"
                } else {
                    "def add(a, b): return a + b"
                };
                let usage = serde_json::json!({"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30});
                if body["stream"] == true {
                    let chunk = serde_json::json!({
                        "model": "gpt-5.6-luna",
                        "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}],
                        "usage": usage,
                    });
                    ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
                } else {
                    let response = serde_json::json!({
                        "model": "gpt-5.6-luna",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                        "usage": usage,
                    });
                    ([("content-type", "application/json")], response.to_string())
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("providers.toml");
    std::fs::write(&config, format!(
        "[providers.openai]\nbase_url = \"http://{address}/v1\"\nchat_endpoint = \"/chat/completions\"\nauth_style = \"none\"\nauth_env = []\n"
    )).unwrap();

    let report = run(temp.path(), &config, &["--live"]).await;
    assert_eq!(report["initial_microusd"], 2_400_000);
    let runs = report["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4);
    for (run, (arm, role)) in runs.iter().zip([
        ("baseline", "actor"),
        ("baseline", "grader"),
        ("candidate", "actor"),
        ("candidate", "grader"),
    ]) {
        assert_eq!(run["allocation"]["arm"], arm);
        assert_eq!(run["allocation"]["role"], role);
        assert_eq!(run["report"]["cost"]["provider_call_count"], 1);
        assert!(run["report"]["cost"]["cost_usd"].as_f64().unwrap() > 0.0);
    }
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4, "all four allocations must reach HTTP");
        for request in requests.iter().skip(1).step_by(2) {
            assert!(
                request["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .contains("def add(a, b): return a + b")
            );
        }
    }
    let refused = run(
        temp.path(),
        &config,
        &["--call", "actor", "10000", "Return hello"],
    )
    .await;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["admission_reason"], "insufficient_allowance");
    assert_eq!(refused["cost"]["provider_call_count"], 0);
    assert_eq!(refused["cost"]["admission"]["denied_attempts"], 1);
    assert_eq!(refused["cost"]["admission"]["uncertain_usd"], "0");
    assert_eq!(
        requests.lock().unwrap().len(),
        4,
        "refusal must not dispatch"
    );
    server.abort();
}
