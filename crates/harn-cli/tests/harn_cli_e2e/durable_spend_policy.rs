//! Real CLI processes share one allowance before reaching a local provider.
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::response::IntoResponse;
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

async fn judged_agent_provider(
    root: &Path,
    actor_priced: bool,
) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        let call = counter.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            let content = if call % 2 == 1 {
                "<user_response>ready</user_response>\n<done>##DONE##</done>".to_string()
            } else {
                let verdict = if actor_priced && call == 2 {
                    "continue"
                } else {
                    "done"
                };
                let gap = if verdict == "continue" {
                    "other"
                } else {
                    "none"
                };
                serde_json::json!({
                    "verdict": verdict, "detail": "Reply ready.", "gap_class": gap,
                    "gaps": [], "goal_assessment": {"kind": "unknown", "evidence_refs": []}
                })
                .to_string()
            };
            if body["stream"].as_bool() == Some(true) {
                let chunk = serde_json::json!({"model": body["model"], "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]});
                let terminal = serde_json::json!({"model": body["model"], "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
                return (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: {terminal}\n\ndata: [DONE]\n\n"),
                )
                    .into_response();
            }
            let mut response = serde_json::json!({"model": body["model"], "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]});
            if actor_priced && call == 1 {
                response["usage"] = serde_json::json!({"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2});
            }
            axum::Json(response).into_response()
        }
    };
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(handler));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(root.join("providers.toml"), format!("[providers.openai]\nbase_url = \"http://{address}/v1\"\nauth_env = \"SPEND_FIXTURE_KEY\"\n")).unwrap();
    (calls, server)
}

async fn invoke_isolated(
    project: &Path,
    authority: &Path,
    transcript_dir: Option<&Path>,
) -> std::process::Output {
    let project = project.to_path_buf();
    let authority = authority.to_path_buf();
    let transcript_dir = transcript_dir.map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || {
        let home = authority.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut command = harn_e2e_command();
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(project)
            .env("HOME", home)
            .env("HARN_PROVIDERS_CONFIG", authority.join("providers.toml"))
            .env("HARN_SECRET_PROVIDERS", "env")
            .env("HARN_SPEND_POLICY", authority.join("policy.toml"))
            .env("SPEND_FIXTURE_KEY", "local-fixture-only")
            .env_remove("HARN_LLM_CALLS_DISABLED")
            .args(["run", "probe.harn"]);
        if let Some(system_root) = std::env::var_os("SYSTEMROOT") {
            command.env("SYSTEMROOT", system_root);
        }
        if let Some(transcript_dir) = transcript_dir {
            command.env("HARN_LLM_TRANSCRIPT_DIR", transcript_dir);
        }
        command.output().unwrap()
    })
    .await
    .unwrap()
}

async fn agent_spend_guard_case(
    actor_priced: bool,
    strict: bool,
    expected_calls: usize,
    transcript_available: bool,
) {
    let source = r#"import { agent_loop } from "std/agent/loop"
import { AgentSpec } from "std/agent/options"
fn main(harness: Harness) {
  const root = harness.fs.mkdtemp_in_workspace("spend-guard")
  harness.agent.open("spend-guard")
  const opts: AgentSpec = {
    provider: "openai", model: "gpt-4o-mini", stream: false, root: root,
    session_id: "spend-guard", loop_until_done: true, max_iterations: 4,
    turn_end_condition: true,
  }
  const result = agent_loop(harness, "Reply ready.", nil, opts)
  harness.stdio.println(json_stringify(result))
}"#;
    let started = std::time::Instant::now();
    eprintln!(
            "spend guard case start: actor_priced={actor_priced} strict={strict} transcript_available={transcript_available}"
        );
    let authority = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("probe.harn"), source).unwrap();
    let transcript_dir = authority.path().join("transcript");
    if transcript_available {
        std::fs::create_dir(&transcript_dir).unwrap();
    } else {
        // A regular file cannot be a directory, even for a privileged test user.
        std::fs::write(&transcript_dir, "blocked").unwrap();
    }
    let (calls, server) = judged_agent_provider(authority.path(), actor_priced).await;
    write_policy(authority.path(), 100_000_000);
    let policy = authority.path().join("policy.toml");
    let contents = std::fs::read_to_string(&policy).unwrap();
    std::fs::write(
        &policy,
        contents.replace(
            "[limits]",
            &format!("stop_on_unknown_usage = {strict}\n[limits]"),
        ),
    )
    .unwrap();
    let output = invoke_isolated(project.path(), authority.path(), Some(&transcript_dir)).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("typed agent result");
    assert_eq!(calls.load(Ordering::SeqCst), expected_calls, "{result}");
    let transcript_path = transcript_dir.join("llm_transcript.jsonl");
    if transcript_available {
        let transcript = std::fs::read_to_string(&transcript_path).unwrap();
        assert!(
            transcript.lines().any(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .is_ok_and(|event| event["type"] == "provider_call_request")
            }),
            "positive transcript control did not reach provider request logging"
        );
    } else {
        assert!(
            !transcript_path.exists(),
            "failed transcript unexpectedly exists"
        );
    }
    let receipt = MachineSpendQuota::from_policy_file(&policy)
        .unwrap()
        .receipt()
        .unwrap();
    let expected_unknown = if strict {
        1
    } else if actor_priced {
        3
    } else {
        2
    };
    assert_eq!(receipt.lifetime_usage_unknown_attempts, expected_unknown);
    assert_eq!(receipt.lifetime_unsettled_attempts, expected_unknown);
    assert_eq!(receipt.lifetime_unpriced_attempts, 0);
    if actor_priced {
        assert!(receipt.lifetime_actual_known_microusd > 0);
    }
    if !strict {
        assert_eq!(result["status"], "done", "{result}");
        assert_eq!(result["visible_text"], "ready", "{result}");
    } else {
        assert_ne!(result["status"], "done", "{result}");
    }
    eprintln!(
            "spend guard case complete: actor_priced={actor_priced} strict={strict} transcript_available={transcript_available} elapsed_ms={}",
            started.elapsed().as_millis()
        );
    server.abort();
}

macro_rules! agent_spend_guard_test {
    ($name:ident, $actor_priced:expr, $strict:expr, $calls:expr, $transcript:expr) => {
        #[tokio::test]
        pub(super) async fn $name() {
            agent_spend_guard_case($actor_priced, $strict, $calls, $transcript).await;
        }
    };
}

agent_spend_guard_test!(agent_spend_guard_actor_unknown, false, true, 1, false);
agent_spend_guard_test!(agent_spend_guard_actor_permissive, false, false, 2, false);
agent_spend_guard_test!(agent_spend_guard_judge_unknown, true, true, 2, false);
agent_spend_guard_test!(agent_spend_guard_judge_permissive, true, false, 4, false);
agent_spend_guard_test!(agent_spend_guard_transcript_control, false, false, 2, true);

#[tokio::test]
pub(super) async fn strict_scope_refuses_a_new_process_after_unpriced_provider_usage() {
    let authority = tempfile::tempdir().unwrap();
    let actor = tempfile::tempdir().unwrap();
    let judge = tempfile::tempdir().unwrap();
    for (project, role) in [
        (actor.path(), "agent.main"),
        (judge.path(), "completion.judge"),
    ] {
        std::fs::write(
            project.join("probe.harn"),
            format!(
                r#"fn main(harness: Harness) {{
  harness.llm.call("hello", nil, {{provider: "openai", model: "gpt-4o-mini", max_tokens: 64, stream: false, call_role: "{role}"}})
  harness.stdio.println("provider_call_completed")
}}"#
            ),
        )
        .unwrap();
    }
    let (calls, _, _, server) = local_provider(authority.path(), false).await;
    write_policy(authority.path(), 2_000_000);
    let policy_path = authority.path().join("policy.toml");
    let source = std::fs::read_to_string(&policy_path).unwrap();
    std::fs::write(
        &policy_path,
        source.replace("[limits]", "stop_on_unknown_usage = true\n[limits]"),
    )
    .unwrap();

    let first = invoke_isolated(actor.path(), authority.path(), None).await;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("provider_call_completed"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let quota = MachineSpendQuota::from_policy_file(&policy_path).unwrap();
    let receipt = quota.receipt().unwrap();
    assert_eq!(receipt.lifetime_usage_unknown_attempts, 1);
    assert!(receipt.lifetime_reserved_microusd > 0);
    assert!(
        receipt.lifetime_remaining_microusd.unwrap() >= receipt.lifetime_reserved_microusd,
        "the second call must fit the remaining allowance without the strict policy"
    );

    let second = invoke_isolated(judge.path(), authority.path(), None).await;
    assert!(
        !second.status.success(),
        "unpriced actor call must stop the judge"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("unresolved_usage"),
        "judge must report the unresolved-usage refusal: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the judge reached the provider"
    );

    // Reverse the roles in a fresh scope so both programs prove they can
    // reach the provider when no earlier unknown exists.
    let source = std::fs::read_to_string(&policy_path).unwrap();
    std::fs::write(
        &policy_path,
        source.replace("scope = \"catalog\"", "scope = \"judge-first\""),
    )
    .unwrap();
    let judge_first = invoke_isolated(judge.path(), authority.path(), None).await;
    assert!(
        judge_first.status.success(),
        "{}",
        String::from_utf8_lossy(&judge_first.stderr)
    );
    assert!(String::from_utf8_lossy(&judge_first.stdout).contains("provider_call_completed"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let actor_second = invoke_isolated(actor.path(), authority.path(), None).await;
    assert!(
        !actor_second.status.success(),
        "unpriced judge call must stop the actor"
    );
    assert!(
        String::from_utf8_lossy(&actor_second.stderr).contains("unresolved_usage"),
        "actor must report the unresolved-usage refusal: {}",
        String::from_utf8_lossy(&actor_second.stderr)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the actor reached the provider"
    );

    let source = std::fs::read_to_string(&policy_path).unwrap();
    std::fs::write(
        &policy_path,
        source
            .replace("scope = \"judge-first\"", "scope = \"permissive-control\"")
            .replace(
                "stop_on_unknown_usage = true",
                "stop_on_unknown_usage = false",
            ),
    )
    .unwrap();
    let actor_control = invoke_isolated(actor.path(), authority.path(), None).await;
    assert!(
        actor_control.status.success(),
        "{}",
        String::from_utf8_lossy(&actor_control.stderr)
    );
    let judge_control = invoke_isolated(judge.path(), authority.path(), None).await;
    assert!(
        judge_control.status.success(),
        "{}",
        String::from_utf8_lossy(&judge_control.stderr)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "both permissive controls must reach the provider"
    );
    server.abort();
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
