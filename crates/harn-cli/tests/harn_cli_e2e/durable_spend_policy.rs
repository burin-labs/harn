//! Real CLI processes share one allowance before reaching a local provider.
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use harn_vm::llm::{MachineSpendPolicy, MachineSpendQuota};

use crate::test_util::process::harn_e2e_command;

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
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        counter.fetch_add(1, Ordering::SeqCst);
        let held = held.clone();
        let released = released.clone();
        async move {
            if held.load(Ordering::SeqCst) {
                released.notified().await;
            }
            // Deliberately omit usage: a restart must keep the reservation.
            axum::Json(serde_json::json!({
                "model": body["model"],
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                             "finish_reason": "stop"}]
            }))
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
    let (calls, hold_transport, release, server) = local_provider(authority.path()).await;
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
