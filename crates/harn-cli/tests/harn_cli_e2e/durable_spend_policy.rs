//! Real CLI and local HTTP: unknown usage remains charged after a restart.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};
use harn_vm::llm::{MachineSpendPolicy, MachineSpendQuota};

fn write_policy(root: &std::path::Path, limit: i64) {
    std::fs::write(
        root.join("policy.toml"),
        format!(
            "ledger_path = {}\nscope = \"catalog\"\n[limits]\nlifetime_limit_microusd = {limit}\n",
            toml::Value::String(root.join("spend.sqlite").to_string_lossy().into_owned())
        ),
    )
    .unwrap();
}

async fn invoke(
    root: &std::path::Path,
    shorthand: bool,
    args: Vec<String>,
) -> std::process::Output {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = harn_e2e_command();
        command
            .current_dir(&root)
            .env("HARN_PROVIDERS_CONFIG", root.join("providers.toml"))
            .env("HARN_SECRET_PROVIDERS", "env")
            .env("HARN_SPEND_POLICY", root.join("policy.toml"))
            .env("PROBE_PROVIDER_KEY", "local-fixture-only")
            .env_remove("HARN_LLM_CALLS_DISABLED");
        if !shorthand {
            command.arg("run");
        }
        command.args(args).output().unwrap()
    })
    .await
    .unwrap()
}

async fn local_provider(
    root: &std::path::Path,
    response_text: &'static str,
    response_status: axum::http::StatusCode,
) -> (
    Arc<AtomicUsize>,
    Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        counter.fetch_add(1, Ordering::SeqCst);
        captured.lock().unwrap().push(body.clone());
        async move {
            let model = body["model"].clone();
            if body.get("input").is_some() {
                let reply = serde_json::json!({"id": "fixture", "model": model,
                        "status": "completed", "output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": response_text}]}]});
                (
                    response_status,
                    [("content-type", "application/json")],
                    reply.to_string(),
                )
            } else if body["stream"] == serde_json::json!(true) {
                let chunk = serde_json::json!({"model": model, "choices": [{"index": 0,
                        "delta": {"content": response_text}, "finish_reason": "stop"}]});
                (
                    response_status,
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                )
            } else {
                let reply = serde_json::json!({"model": model, "choices": [{"index": 0,
                        "message": {"role": "assistant", "content": response_text}, "finish_reason": "stop"}]});
                (
                    response_status,
                    [("content-type", "application/json")],
                    reply.to_string(),
                )
            }
        }
    };
    let app = axum::Router::new()
        .route("/v1/chat/completions", axum::routing::post(handler.clone()))
        .route("/v1/responses", axum::routing::post(handler));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(root.join("providers.toml"), format!(
        "[providers.openai]\nbase_url = \"http://{address}/v1\"\nauth_env = \"PROBE_PROVIDER_KEY\"\n"
    )).unwrap();
    (calls, requests, server)
}

#[tokio::test]
async fn restarted_cli_retains_unknown_usage_and_refuses_before_http() {
    let root = tempfile::tempdir().unwrap();
    let (calls, _, server) = local_provider(root.path(), "ok", axum::http::StatusCode::OK).await;
    std::fs::write(
        root.path().join("probe.harn"),
        r#"fn main(harness: Harness) {
        harness.llm.call("hello", nil, {provider: "openai", model: "gpt-5.6-luna", max_tokens: 64})
        harness.stdio.log("provider_call_completed")
    }"#,
    )
    .unwrap();
    write_policy(root.path(), 2_000_000);
    let first = invoke(root.path(), false, vec!["probe.harn".into()]).await;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("provider_call_completed"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut policy = MachineSpendPolicy {
        daily_limit_microusd: None,
        monthly_limit_microusd: None,
        lifetime_limit_microusd: Some(2_000_000),
    };
    let quota =
        MachineSpendQuota::open(root.path().join("spend.sqlite"), "catalog", policy.clone())
            .unwrap();
    let receipt = quota.receipt().unwrap();
    assert!(receipt.lifetime_reserved_microusd > 0);
    assert_eq!(receipt.lifetime_actual_known_microusd, 0);
    assert_eq!(receipt.lifetime_usage_unknown_attempts, 1);
    let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/provider_tool_probe_campaign/types.harn");
    let mut parent = format!(
        "import {{provider_tool_probe_campaign_command_env}} from {}\n",
        serde_json::to_string(&module.to_string_lossy()).unwrap()
    );
    parent.push_str(
        r#"fn main(harness: Harness) {
          const env = provider_tool_probe_campaign_command_env(
            argv[1] + "/providers.toml", harness.env.get_or("HARN_SPEND_POLICY", ""), true
          ) + {PROBE_PROVIDER_KEY: "local-fixture-only", HARN_SECRET_PROVIDERS: "env"}
          const child = harness.process.run({
            program: argv[0], args: ["run", "probe.harn"], cwd: argv[1], env: env, timeout_ms: 30000
          })
          guard child.exit_code == 0 else { throw child.stderr }
          harness.stdio.println("campaign_child_completed")
        }"#,
    );
    std::fs::write(root.path().join("parent.harn"), parent).unwrap();
    let child = invoke(
        root.path(),
        false,
        vec![
            "--allow-process-network".into(),
            "parent.harn".into(),
            "--".into(),
            harn_e2e_binary().to_string_lossy().into_owned(),
            root.path().to_string_lossy().into_owned(),
        ],
    )
    .await;
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(String::from_utf8_lossy(&child.stdout).contains("campaign_child_completed"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let child_receipt = quota.receipt().unwrap();
    assert_eq!(child_receipt.lifetime_usage_unknown_attempts, 2);
    assert_eq!(child_receipt.lifetime_actual_known_microusd, 0);
    assert!(child_receipt.lifetime_reserved_microusd > receipt.lifetime_reserved_microusd);
    policy.lifetime_limit_microusd = Some(child_receipt.lifetime_reserved_microusd);
    quota.update_policy(policy, "test-host").unwrap();
    write_policy(root.path(), child_receipt.lifetime_reserved_microusd);
    let restarted = invoke(root.path(), true, vec!["probe.harn".into()]).await;
    server.abort();
    assert!(!restarted.status.success());
    assert!(
        String::from_utf8_lossy(&restarted.stderr).contains("budget_exceeded"),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "denied restart reached HTTP"
    );
    assert_eq!(
        quota.receipt().unwrap().lifetime_reserved_microusd,
        child_receipt.lifetime_reserved_microusd
    );
}

#[tokio::test]
async fn script_tool_probe_keeps_budget_ledger_outside_agent_write_roots() {
    let workspace = tempfile::tempdir().unwrap();
    let authority = tempfile::tempdir().unwrap();
    let (calls, _, server) =
        local_provider(workspace.path(), "ok", axum::http::StatusCode::OK).await;
    write_policy(authority.path(), 2_000_000);
    let ledger = authority.path().join("spend.sqlite");
    let policy = authority.path().join("policy.toml");
    std::fs::write(
        workspace.path().join("mismatch.harn"),
        r#"
      fn main(harness: Harness) {
        harness.llm.tool_probe({provider: "anthropic", model: "gpt-5.6-luna"})
      }
    "#,
    )
    .unwrap();
    let mismatch = invoke(
        workspace.path(),
        false,
        vec![
            "--spend-policy".into(),
            policy.to_string_lossy().into_owned(),
            "mismatch.harn".into(),
        ],
    )
    .await;
    assert!(
        !mismatch.status.success(),
        "a contradictory provider/model pair was accepted: {}",
        String::from_utf8_lossy(&mismatch.stdout)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    std::fs::write(
        workspace.path().join("probe.harn"),
        r#"
      fn main(harness: Harness) {
        const report = harness.llm.tool_probe({
          provider: "openai", model: "gpt-5.6-luna", modes: ["non_streaming"],
          max_cost_usd: 1.0, timeout_secs: 10
        })
        const tamper = try { harness.fs.write_text(argv[0], "corruption") }
        guard is_err(tamper) else { throw "agent could overwrite the host budget ledger" }
        harness.stdio.println(json_stringify(report))
      }
    "#,
    )
    .unwrap();
    let args = vec![
        "--spend-policy".into(),
        policy.to_string_lossy().into_owned(),
        "probe.harn".into(),
        "--".into(),
        ledger.to_string_lossy().into_owned(),
    ];
    let first = invoke(workspace.path(), false, args.clone()).await;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["evidence_source"], "live_request");
    assert_eq!(report["cases"].as_array().unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{report}");
    let quota = MachineSpendQuota::open(
        &ledger,
        "catalog",
        MachineSpendPolicy {
            daily_limit_microusd: None,
            monthly_limit_microusd: None,
            lifetime_limit_microusd: Some(2_000_000),
        },
    )
    .unwrap();
    let charged = quota.receipt().unwrap();
    assert!(charged.lifetime_reserved_microusd > 0);
    assert_eq!(charged.lifetime_usage_unknown_attempts, 1);
    write_policy(authority.path(), charged.lifetime_reserved_microusd);
    let unauthorized = invoke(workspace.path(), false, args.clone()).await;
    assert!(!unauthorized.status.success());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    quota
        .update_policy(
            MachineSpendPolicy {
                daily_limit_microusd: None,
                monthly_limit_microusd: None,
                lifetime_limit_microusd: Some(charged.lifetime_reserved_microusd),
            },
            "test-host",
        )
        .unwrap();
    let denied = invoke(workspace.path(), false, args).await;
    assert!(
        denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(report["admission"]["denied_attempts"], 1, "{report}");
    assert_eq!(
        quota.receipt().unwrap().lifetime_reserved_microusd,
        charged.lifetime_reserved_microusd
    );
    server.abort();
}

#[tokio::test]
async fn script_option_probe_preserves_other_guards_and_makes_one_request() {
    let root = tempfile::tempdir().unwrap();
    let (calls, requests, server) =
        local_provider(root.path(), "", axum::http::StatusCode::OK).await;
    write_policy(root.path(), 2_000_000);
    std::fs::write(
        root.path().join("option.harn"),
        r#"
      import { provider_option_probe } from "std/llm/option_probe"
      fn main(harness: Harness) {
        const before = provider_option_probe(
          harness.llm, "openai", "gpt-5.6-luna", "temperature", 8, false
        )
        guard before.probe.verdict == "gated_locally" && before.probe.request_count == 0 else {
          throw "ordinary option guard did not fire"
        }
        const probe = provider_option_probe(
          harness.llm, "openai", "gpt-5.6-luna", "temperature"
        )
        guard probe.probe.verdict == "accepted" && probe.probe.request_count == 1 else {
          throw json_stringify(probe)
        }
        const after = provider_option_probe(
          harness.llm, "openai", "gpt-5.6-luna", "temperature", 8, false
        )
        guard after.probe.verdict == "gated_locally" else { throw "probe authority escaped" }
        const unrelated = try {
          harness.llm.option_probe_call("ok", "temperature", {
            provider: "openai", model: "gpt-5.6-luna", max_tokens: 8,
            stream: false, temperature: 0.2, top_k: 1
          })
        }
        guard is_err(unrelated) else { throw "unselected option guard was bypassed" }
        harness.stdio.println(json_stringify(probe))
      }
    "#,
    )
    .unwrap();
    let output = invoke(root.path(), false, vec!["option.harn".into()]).await;
    server.abort();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["probe"]["verdict"], "accepted");
    assert_eq!(report["probe"]["attempt"]["reason"], "served_empty");
    assert_eq!(report["diff"]["status"], "drift");
    let requests = requests.lock().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{requests:?}");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["temperature"], 0.2);
}

#[tokio::test]
async fn script_option_probe_does_not_repeat_for_output_validation() {
    let root = tempfile::tempdir().unwrap();
    let (calls, requests, server) =
        local_provider(root.path(), "{}", axum::http::StatusCode::OK).await;
    write_policy(root.path(), 2_000_000);
    std::fs::write(
        root.path().join("schema.harn"),
        r#"fn main(harness: Harness) {
          const probe = try {
            harness.llm.option_probe_call("Reply with JSON", "temperature", {
              provider: "openai", model: "gpt-5.6-luna", max_tokens: 8,
              stream: false, temperature: 0.2, schema_retries: 2,
              output: {schema: {type: "object", required: ["ok"],
                properties: {ok: {type: "boolean"}}}, strict: true, validation: "error"}
            })
          }
          guard is_err(probe) else { throw "invalid output passed validation" }
        }"#,
    )
    .unwrap();
    let output = invoke(root.path(), false, vec!["schema.harn".into()]).await;
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = requests.lock().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{requests:?}");
}

#[tokio::test]
async fn script_option_probe_does_not_race_or_fail_over() {
    let root = tempfile::tempdir().unwrap();
    let (calls, requests, server) = local_provider(
        root.path(),
        "fixture failure",
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    write_policy(root.path(), 2_000_000);
    std::fs::write(
        root.path().join("routing.harn"),
        r#"fn main(harness: Harness) {
          const policy = harness.llm.routing_policy({
            chain: [{provider: "openai", model: "gpt-5.6-luna"},
              {provider: "openai", model: "gpt-5.4-mini"}],
            failover: {on_status: [500], max_attempts: 3},
            latency: {race_after_ms: 1}
          })
          const probe = try {
            harness.llm.option_probe_call("ok", "temperature", {
              provider: "openai", model: "gpt-5.6-luna", max_tokens: 8,
              stream: false, temperature: 0.2, routing: policy
            })
          }
          guard is_err(probe) else { throw "failed provider was accepted" }
        }"#,
    )
    .unwrap();
    let output = invoke(root.path(), false, vec!["routing.harn".into()]).await;
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = requests.lock().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{requests:?}");
    assert_eq!(requests[0]["model"], "gpt-5.6-luna");
}
