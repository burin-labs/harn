//! Real CLI processes share one allowance before reaching a local provider.
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use harn_vm::llm::{MachineSpendPolicy, MachineSpendQuota};

use crate::test_util::process::harn_e2e_command;
use crate::test_util::stdio_jsonrpc::StdioJsonRpcClient;

async fn invoke_acp(project: &Path, authority: &Path) -> serde_json::Value {
    let project = project.to_path_buf();
    let authority = authority.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = harn_e2e_command();
        command
            .current_dir(&project)
            .env("HARN_PROVIDERS_CONFIG", authority.join("providers.toml"))
            .env("HARN_SECRET_PROVIDERS", "env")
            .env("HARN_SPEND_POLICY", authority.join("policy.toml"))
            .env("SPEND_FIXTURE_KEY", "local-fixture-only")
            .env_remove("HARN_LLM_CALLS_DISABLED")
            .args(["serve", "acp", "probe.harn"]);
        let mut client = StdioJsonRpcClient::spawn("quota ACP", command);
        let created = client.request(serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"session/new",
            "params":{"cwd":project, "environmentPolicy":{"kind":"inherited"}}
        }));
        let session = created["result"]["sessionId"]
            .as_str()
            .expect("ACP session");
        client.send(&serde_json::json!({
            "jsonrpc":"2.0", "method":"session/set_budget",
            "params":{"sessionId":session,"llm_cost_usd":5.0,"llm_tokens":null}
        }));
        let turn = client.exchange(
            serde_json::json!({
                "jsonrpc":"2.0", "id":2, "method":"session/prompt",
                "params":{"sessionId":session,"prompt":[{"type":"text","text":"go"}]}
            }),
            |method| {
                assert_eq!(method, "host/capabilities");
                serde_json::json!({})
            },
        );
        let output: String = turn
            .notifications
            .iter()
            .filter(|message| message["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
            .filter_map(|message| message["params"]["update"]["content"]["text"].as_str())
            .collect();
        let failure = turn.response.get("error").map(|error| {
            assert_eq!(error["data"]["schema"], "harn.acp.prompt_error.v1");
            assert_eq!(error["data"]["category"], "budget_exceeded", "{error}");
            error["data"]["machineSpend"].clone()
        });
        client.shutdown_expect_success();
        failure.unwrap_or_else(|| serde_json::from_str(output.trim()).expect("typed quota outcome"))
    })
    .await
    .unwrap()
}

fn write_policy(root: &Path, limit: i64) {
    std::fs::write(
        root.join("policy.toml"),
        format!(
            "ledger_path = {}\nscope = \"catalog\"\n[limits]\nlifetime_limit_microusd = {limit}\n",
            toml::Value::String(root.join("spend.sqlite").to_string_lossy().into_owned())
        ),
    )
    .unwrap();
}

async fn invoke(project: &Path, authority: &Path, shorthand: bool) -> std::process::Output {
    let project = project.to_path_buf();
    let authority = authority.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = harn_e2e_command();
        command
            .current_dir(project)
            .env("HARN_PROVIDERS_CONFIG", authority.join("providers.toml"))
            .env("HARN_SECRET_PROVIDERS", "env")
            .env("HARN_SPEND_POLICY", authority.join("policy.toml"))
            .env("SPEND_FIXTURE_KEY", "local-fixture-only")
            .env_remove("HARN_LLM_CALLS_DISABLED");
        if !shorthand {
            command.arg("run");
        }
        command.arg("probe.harn").output().unwrap()
    })
    .await
    .unwrap()
}

async fn local_provider(
    root: &Path,
    metered: bool,
) -> (
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let hold_transport = Arc::new(AtomicBool::new(false));
    let held = hold_transport.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let released = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let model = harn_vm::llm_config::model_catalog_entry("gpt-4o-mini").unwrap();
    let input_tokens = model
        .context_window
        .max(model.runtime_context_window.unwrap_or(0));
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        let call = counter.fetch_add(1, Ordering::SeqCst) + 1;
        let held = held.clone();
        let released = released.clone();
        async move {
            // A broken allocator admits both contenders. Release the held
            // request so that case fails its census instead of deadlocking.
            if call > 2 && held.swap(false, Ordering::SeqCst) {
                released.notify_one();
            }
            if held.load(Ordering::SeqCst) {
                released.notified().await;
            }
            let mut response = serde_json::json!({
                "model": body["model"],
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                             "finish_reason": "stop"}]
            });
            if metered {
                // Charge the catalog bound so the concurrent processes cannot
                // pass merely because reported usage was cheaper than the hold.
                let output_tokens = body["max_tokens"].as_u64().unwrap();
                response["usage"] = serde_json::json!({
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": input_tokens + output_tokens,
                });
            }
            axum::Json(response)
        }
    };
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(handler));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(
        root.join("providers.toml"),
        format!("[providers.openai]\nbase_url = \"http://{address}/v1\"\nauth_env = \"SPEND_FIXTURE_KEY\"\n"),
    )
    .unwrap();
    (calls, hold_transport, release, server)
}

#[tokio::test]
async fn concurrent_projects_and_restarted_shorthand_share_the_host_allowance() {
    let authority = tempfile::tempdir().unwrap();
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let source = r#"fn main(harness: Harness) {
        harness.llm.call("hello", nil, {provider: "openai", model: "gpt-4o-mini", max_tokens: 64, stream: false})
        harness.stdio.println("provider_call_completed")
    }"#;
    for project in [left.path(), right.path()] {
        std::fs::write(project.join("probe.harn"), source).unwrap();
    }
    // Missing usage must retain the full reservation across restart.
    let (calls, hold_transport, release, server) = local_provider(authority.path(), false).await;
    write_policy(authority.path(), 2_000_000);
    let first = invoke(left.path(), authority.path(), false).await;
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
    let quota = MachineSpendQuota::open(
        authority.path().join("spend.sqlite"),
        "catalog",
        policy.clone(),
    )
    .unwrap();
    let held = quota.receipt().unwrap();
    assert!(held.lifetime_reserved_microusd > 0);
    assert_eq!(held.lifetime_actual_known_microusd, 0);
    assert_eq!(held.lifetime_usage_unknown_attempts, 1);
    let limit = held.lifetime_reserved_microusd.checked_mul(2).unwrap();
    policy.lifetime_limit_microusd = Some(limit);
    quota.update_policy(policy, "fixture-host").unwrap();
    write_policy(authority.path(), limit);

    // Keep the admitted transport active until its competing process is
    // refused. A sequential replay can't satisfy this overlap protocol.
    hold_transport.store(true, Ordering::SeqCst);
    let competing = |project: std::path::PathBuf, shorthand| {
        let release = release.clone();
        let authority = authority.path().to_path_buf();
        async move {
            let result = invoke(&project, &authority, shorthand).await;
            if !result.status.success() {
                release.notify_one();
            }
            result
        }
    };
    let (a, b) = tokio::join!(
        competing(left.path().to_path_buf(), false),
        competing(right.path().to_path_buf(), true),
    );
    assert_ne!(
        a.status.success(),
        b.status.success(),
        "exactly one concurrent call must be admitted: {a:?} {b:?}"
    );
    let refused = if a.status.success() { &b } else { &a };
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("budget_exceeded"),
        "{refused:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "refused process reached HTTP"
    );
    let charged = quota.receipt().unwrap();
    assert_eq!(charged.lifetime_reserved_microusd, limit);
    assert_eq!(charged.lifetime_actual_known_microusd, 0);
    assert_eq!(charged.lifetime_usage_unknown_attempts, 2);

    let restarted = invoke(right.path(), authority.path(), true).await;
    server.abort();
    assert!(
        !restarted.status.success(),
        "restart reset the durable allowance"
    );
    assert!(
        String::from_utf8_lossy(&restarted.stderr).contains("budget_exceeded"),
        "{restarted:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "denied restart reached HTTP"
    );
    assert_eq!(quota.receipt().unwrap().lifetime_reserved_microusd, limit);
}

#[tokio::test]
async fn cli_and_acp_share_a_monthly_quota_across_processes_and_restart() {
    let authority = tempfile::tempdir().unwrap();
    let terminal = tempfile::tempdir().unwrap();
    let editor = tempfile::tempdir().unwrap();
    std::fs::write(terminal.path().join("probe.harn"), r#"fn main(harness: Harness) {
        harness.llm.call("hello", nil, {provider: "openai", model: "gpt-4o-mini", max_tokens: 64, stream: false})
        harness.stdio.println("provider_call_completed")
    }"#).unwrap();
    std::fs::write(editor.path().join("probe.harn"), r#"pipeline main(harness: Harness) {
        const outcome = try {
            harness.llm.call("hello", nil, {provider: "openai", model: "gpt-4o-mini", max_tokens: 64, stream: false})
            {admitted: true}
        } catch (error) { error }
        harness.stdio.println(json_stringify(outcome))
    }"#).unwrap();
    let (calls, hold, release, server) = local_provider(authority.path(), true).await;
    let write_monthly = |limit| {
        std::fs::write(authority.path().join("policy.toml"), format!(
            "ledger_path = {}\nscope = \"person\"\n[limits]\nmonthly_limit_microusd = {limit}\n",
            toml::Value::String(authority.path().join("spend.sqlite").to_string_lossy().into_owned())
        )).unwrap();
    };
    write_monthly(2_000_000);
    let positive = invoke_acp(editor.path(), authority.path()).await;
    assert_eq!(positive["admitted"], true, "{positive}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut policy = MachineSpendPolicy {
        daily_limit_microusd: None,
        monthly_limit_microusd: Some(2_000_000),
        lifetime_limit_microusd: None,
    };
    let quota = MachineSpendQuota::open(
        authority.path().join("spend.sqlite"),
        "person",
        policy.clone(),
    )
    .unwrap();
    let one_call = quota.receipt().unwrap().lifetime_reserved_microusd;
    assert!(one_call > 0, "positive control must reserve real allowance");
    assert_eq!(
        quota.receipt().unwrap().lifetime_actual_known_microusd,
        one_call
    );
    assert_eq!(quota.receipt().unwrap().lifetime_usage_unknown_attempts, 0);
    let limit = one_call * 2;
    policy.monthly_limit_microusd = Some(limit);
    quota
        .update_policy(policy.clone(), "fixture-host-approved")
        .unwrap();
    write_monthly(limit);
    hold.store(true, Ordering::SeqCst);
    let acp = async {
        let outcome = invoke_acp(editor.path(), authority.path()).await;
        if outcome["admitted"] != true {
            release.notify_one();
        }
        outcome
    };
    let cli = async {
        let outcome = invoke(terminal.path(), authority.path(), false).await;
        if !outcome.status.success() {
            release.notify_one();
        }
        outcome
    };
    let (acp, cli) = tokio::join!(acp, cli);
    assert_ne!(
        acp["admitted"] == true,
        cli.status.success(),
        "{acp} {cli:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(quota.receipt().unwrap().lifetime_reserved_microusd, limit);
    assert_eq!(
        quota.receipt().unwrap().lifetime_actual_known_microusd,
        limit
    );
    assert_eq!(quota.receipt().unwrap().lifetime_usage_unknown_attempts, 0);
    hold.store(false, Ordering::SeqCst);
    std::fs::write(editor.path().join("probe.harn"), r#"pipeline main(harness: Harness) {
        harness.llm.call("hello", nil, {provider: "openai", model: "gpt-4o-mini", max_tokens: 64, stream: false})
    }"#).unwrap();
    let restarted = invoke_acp(editor.path(), authority.path()).await;
    assert_eq!(restarted["period"], "month", "{restarted}");
    assert_eq!(restarted["remaining_microusd"], 0);
    assert!(restarted["reset_at_unix_ms"].as_i64().unwrap() > 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "restart bypassed quota");
    let reopened =
        MachineSpendQuota::open(authority.path().join("spend.sqlite"), "person", policy).unwrap();
    assert_eq!(
        reopened.receipt().unwrap().lifetime_reserved_microusd,
        limit
    );
    server.abort();
}
