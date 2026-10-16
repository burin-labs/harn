//! Canonical option probes against a changing, credential-free routing pool.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use serde_json::{json, Value};

use crate::test_util::process::harn_e2e_command;

type Reply = (StatusCode, Value);

struct Pool {
    config: std::path::PathBuf,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Pool {
    async fn start(root: &Path) -> Self {
        let replies = Arc::new(Mutex::new(VecDeque::<Reply>::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let pending = replies.clone();
        let recorded = requests.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(
                move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let pending = pending.clone();
                    let recorded = recorded.clone();
                    async move {
                        assert!(headers.get("authorization").is_none());
                        assert!(!body.get("stream").and_then(Value::as_bool).unwrap_or(false));
                        assert_eq!(body["provider"]["require_parameters"], true);
                        assert_eq!(
                            body["messages"][0]["content"],
                            "Reply with the single word: ok"
                        );
                        recorded.lock().unwrap().push(body);
                        let (status, response) = pending
                            .lock()
                            .unwrap()
                            .pop_front()
                            .expect("unexpected provider request");
                        (status, axum::Json(response))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = root.join("providers.toml");
        std::fs::write(
            &config,
            format!(
                r#"[providers.openrouter]
base_url = "http://{address}/v1"
chat_endpoint = "/chat/completions"
auth_style = "none"
auth_env = []
"#
            ),
        )
        .unwrap();
        Self {
            config,
            replies,
            requests,
            server,
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn accepted(model: Option<&str>, text: &str) -> Reply {
    let mut response = json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 4, "completion_tokens": 1, "cost": 0.000001}
    });
    if let Some(model) = model {
        response["model"] = json!(model);
    }
    (StatusCode::OK, response)
}

fn rejected() -> Reply {
    (
        StatusCode::NOT_FOUND,
        json!({"error": {
            "code": 404,
            "message": "No endpoints found that can handle the requested parameters.",
            "metadata": {"failed_routing_step": "Filter by Parameters"}
        }}),
    )
}

async fn invoke_probe(
    root: &Path,
    pool: &Pool,
    model: &str,
    replies: Vec<Reply>,
) -> (Output, Value) {
    invoke_probe_with_mode(root, pool, model, "top_k", false, replies).await
}

async fn invoke_probe_with_mode(
    root: &Path,
    pool: &Pool,
    model: &str,
    option: &str,
    gated: bool,
    replies: Vec<Reply>,
) -> (Output, Value) {
    let before = pool.requests.lock().unwrap().len();
    let expected = replies.len();
    assert!(pool.replies.lock().unwrap().is_empty());
    pool.replies.lock().unwrap().extend(replies);
    let mut command = harn_e2e_command();
    command
        .current_dir(root)
        .args([
            "provider",
            "option-probe",
            "openrouter",
            "--model",
            model,
            "--option",
            option,
            "--max-tokens",
            "8",
            "--fail-on-drift",
            "--json",
        ])
        .env("HARN_PROVIDERS_CONFIG", &pool.config)
        .env("HARN_LLM_CALLS_DISABLED", "0")
        .env("HARN_LLM_RATE_LIMIT_DURABLE", "0")
        .env("NO_COLOR", "1");
    if gated {
        command.arg("--gated");
    }
    for name in [
        "HARN_LLM_PROVIDER",
        "HARN_LLM_MODEL",
        "HARN_LLM_MOCK_RECORD",
        "HARN_LLM_MOCK_REPLAY",
        "OPENROUTER_API_KEY",
    ] {
        command.env_remove(name);
    }
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(report["schema_version"], "harn.provider_option_probe.v2");
    assert_eq!(report["probe"]["request_count"], expected);
    assert!(pool.replies.lock().unwrap().is_empty());
    let requests = pool.requests.lock().unwrap();
    assert_eq!(requests.len() - before, expected);
    for (index, body) in requests[before..].iter().enumerate() {
        assert_eq!(body["model"], model);
        if index == 0 {
            assert!(
                body.get(option).is_some(),
                "selected option must reach transport"
            );
            if option == "top_k" {
                assert_eq!(body[option], 1);
            }
        } else {
            assert!(
                body.get(option).is_none(),
                "the rejection control must omit the option"
            );
        }
    }
    (output, report)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn measured_catalog_options_admit_supported_requests_and_gate_unsupported_ones() {
    let root = tempfile::tempdir().unwrap();
    let pool = Pool::start(root.path()).await;
    let model = "nex-agi/nex-n2.5-pro";
    let (output, report) = invoke_probe_with_mode(
        root.path(),
        &pool,
        model,
        "top_k",
        true,
        vec![accepted(Some(model), "ok")],
    )
    .await;
    assert!(output.status.success());
    assert_eq!(report["probe"]["ungated"], false);
    assert_eq!(report["catalog"]["claimed_supported"], true);
    assert_eq!(report["probe"]["attempt"]["response_model"], model);
    assert_eq!(report["diff"]["status"], "match");
    let (output, report) =
        invoke_probe_with_mode(root.path(), &pool, model, "stop", true, vec![]).await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(report["catalog"]["claimed_supported"], false);
    assert_eq!(report["probe"]["verdict"], "gated_locally");
    assert_eq!(report["probe"]["measured_count"], 0);
    assert_eq!(report["diff"]["status"], "unmeasured");
    assert_eq!(pool.requests.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_pool_qualification_retains_distinct_selections_and_controlled_unavailability() {
    let root = tempfile::tempdir().unwrap();
    let pool = Pool::start(root.path()).await;
    for backend in ["pool-backend-a", "pool-backend-b"] {
        let (output, report) = invoke_probe(
            root.path(),
            &pool,
            "openrouter/free",
            vec![accepted(Some(backend), "qualification-positive")],
        )
        .await;
        assert!(output.status.success());
        assert_eq!(report["catalog"]["qualification"], "changing_pool");
        assert_eq!(report["diff"]["status"], "match");
        assert_eq!(report["diff"]["observed_supported"], true);
        assert_eq!(report["probe"]["attempt"]["response_model"], backend);
        assert_eq!(report["probe"]["attempt"]["input_tokens"], 4);
        let (output, report) = invoke_probe(
            root.path(),
            &pool,
            "openrouter/free",
            vec![
                rejected(),
                accepted(Some("control-backend"), "qualification-positive"),
            ],
        )
        .await;
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(report["diff"]["status"], "unavailable");
        assert!(report["diff"]["observed_supported"].is_null());
        assert_eq!(report["probe"]["measured_count"], 0);
        assert_eq!(
            report["probe"]["attempt"]["control_response_model"],
            "control-backend"
        );
        assert!(report["probe"]["attempt"]["response_model"].is_null());
    }
    assert_eq!(pool.requests.lock().unwrap().len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_pool_requires_probe_and_control_backend_selection() {
    let root = tempfile::tempdir().unwrap();
    let pool = Pool::start(root.path()).await;
    for replies in [
        vec![accepted(None, "qualification-positive")],
        vec![accepted(Some("openrouter/free"), "qualification-positive")],
        vec![rejected(), accepted(None, "qualification-positive")],
        vec![rejected(), accepted(Some("control-backend"), "")],
    ] {
        let (output, report) = invoke_probe(root.path(), &pool, "openrouter/free", replies).await;
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(report["diff"]["status"], "unmeasured");
        assert!(report["diff"]["observed_supported"].is_null());
        assert_eq!(report["probe"]["measured_count"], 0);
    }
    assert_eq!(pool.requests.lock().unwrap().len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualification_preserves_stable_rejections_failed_controls_and_account_privacy() {
    let root = tempfile::tempdir().unwrap();
    let pool = Pool::start(root.path()).await;
    let (output, report) = invoke_probe(
        root.path(),
        &pool,
        "mistralai/mistral-small-2603",
        vec![
            rejected(),
            accepted(Some("stable-backend"), "qualification-positive"),
        ],
    )
    .await;
    assert!(output.status.success());
    assert_eq!(report["catalog"]["qualification"], "stable_endpoint");
    assert_eq!(report["diff"]["observed_supported"], false);
    let (output, report) = invoke_probe(
        root.path(),
        &pool,
        "openrouter/free",
        vec![
            rejected(),
            (
                StatusCode::TOO_MANY_REQUESTS,
                json!({"error": {"code": 429, "message": "rate limited"}}),
            ),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(report["diff"]["status"], "unmeasured");
    assert_eq!(
        report["probe"]["attempt"]["failure_class"],
        "control_failed"
    );
    let (output, report) = invoke_probe(root.path(), &pool, "openrouter/free", vec![(StatusCode::NOT_FOUND, json!({"error": {"code": 404, "message": "0 endpoints out of 1 requested are available matching your guardrail restrictions and data policy."}}))]).await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(report["probe"]["verdict"], "excluded_by_account_policy");
    assert_eq!(report["diff"]["status"], "unmeasured");
    assert!(report["diff"]["observed_supported"].is_null());
    assert_eq!(pool.requests.lock().unwrap().len(), 5);
}
