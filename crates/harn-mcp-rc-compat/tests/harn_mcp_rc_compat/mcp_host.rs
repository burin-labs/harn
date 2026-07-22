//! Integration tests for the supervised MCP host primitive
//! (`harn_vm::mcp_host`, #2504, A.7).
//!
//! These tests drive `harn.mcp.spawn` / `tools` / `call` / `discover` /
//! `reload` / `stop` end-to-end against an in-process fake HTTP MCP
//! server, exercising the dispatch surface that the harn-serve adapter
//! exposes through `harness.mcp.*`. The unit suite in
//! `crates/harn-vm/src/mcp_host.rs` covers the supervision/cache state
//! machine in isolation; this file is the wire-level smoke test that
//! catches integration regressions between the host module and the
//! underlying `mcp` client.

use harn_mcp_rc_compat::fake_server::{spawn_fake_http_server, FakeServerBehavior};
use harn_vm::{mcp_host, reset_thread_local_state};
use serde_json::json;
use tokio::sync::Mutex;

/// Integration tests share the process-global MCP registry, so they
/// must serialize. Holding this guard for the duration of each test
/// keeps the registry's lazy-boot and ref-count semantics from
/// interfering with neighboring tests.
async fn global_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

fn spec_for(name: &str, url: &str) -> serde_json::Value {
    json!({
        "name": name,
        "transport": "http",
        "url": url,
        "protocol_mode": "rc",
    })
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_then_tools_then_call_completes_against_fake_modern_server() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    let server = spawn_fake_http_server(FakeServerBehavior::ModernSuccess).await;
    let url = format!("{}/mcp", server.base_url);

    let id = mcp_host::spawn(spec_for("modern", &url), mcp_host::SpawnOptions::default())
        .await
        .expect("spawn should succeed against the fake server");
    assert_eq!(id, "modern");

    let tools = mcp_host::tools("modern").await.expect("tools should list");
    assert!(!tools.is_empty(), "expected at least one tool");
    let echo = tools
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("echo"))
        .expect("fake server should advertise an `echo` tool");
    assert_eq!(
        echo.get("_mcp_server").and_then(|v| v.as_str()),
        Some("modern"),
        "host should tag each tool with its originating server"
    );

    let result = mcp_host::call("modern", "echo", json!({"message": "hi"}))
        .await
        .expect("call should succeed");
    // The fake server echoes back `ok:<args>` as a single text content
    // block; the unwrapped result is the raw string.
    assert!(
        result.as_str().unwrap_or_default().starts_with("ok:"),
        "expected echoed text, got {result:?}"
    );

    mcp_host::stop("modern").expect("stop should succeed");
    server.shutdown().await;
    reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn status_reflects_active_server_and_clears_on_stop() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    let server = spawn_fake_http_server(FakeServerBehavior::ModernSuccess).await;
    let url = format!("{}/mcp", server.base_url);

    mcp_host::spawn(spec_for("alpha", &url), mcp_host::SpawnOptions::default())
        .await
        .expect("spawn should succeed");

    let snap = mcp_host::status().await;
    let entry = snap
        .iter()
        .find(|s| s.name == "alpha")
        .expect("status should report the spawned server");
    assert!(entry.active, "alpha should be active after eager spawn");
    assert_eq!(entry.restart_count, 0);
    assert_eq!(entry.consecutive_failures, 0);
    assert_eq!(entry.cache_entries, 0);
    assert!(!entry.ejected);

    mcp_host::stop("alpha").expect("stop should succeed");
    let snap_after = mcp_host::status().await;
    assert!(
        snap_after.iter().all(|s| !s.active || s.name != "alpha"),
        "alpha should no longer be active after stop"
    );

    server.shutdown().await;
    reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn discover_returns_tools_across_multiple_servers() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    let server_a = spawn_fake_http_server(FakeServerBehavior::ModernSuccess).await;
    let server_b = spawn_fake_http_server(FakeServerBehavior::ModernSuccess).await;

    mcp_host::spawn(
        spec_for("server-a", &format!("{}/mcp", server_a.base_url)),
        mcp_host::SpawnOptions::default(),
    )
    .await
    .unwrap();
    mcp_host::spawn(
        spec_for("server-b", &format!("{}/mcp", server_b.base_url)),
        mcp_host::SpawnOptions::default(),
    )
    .await
    .unwrap();

    let entries = mcp_host::discover().await.expect("discover should succeed");
    let server_names: std::collections::BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("server")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    assert!(
        server_names.contains("server-a"),
        "discover should include server-a (got {server_names:?})"
    );
    assert!(
        server_names.contains("server-b"),
        "discover should include server-b (got {server_names:?})"
    );
    // Each entry should expose `server`, `tool`, and a `schema` field.
    for entry in &entries {
        if entry.get("error").is_some() {
            continue;
        }
        assert!(entry.get("server").is_some());
        assert!(entry.get("tool").is_some());
        assert!(entry.get("schema").is_some());
    }

    mcp_host::stop("server-a").ok();
    mcp_host::stop("server-b").ok();
    server_a.shutdown().await;
    server_b.shutdown().await;
    reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn reload_drops_active_connection_but_subsequent_call_reconnects() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    let server = spawn_fake_http_server(FakeServerBehavior::ModernSuccess).await;
    let url = format!("{}/mcp", server.base_url);

    mcp_host::spawn(spec_for("hot", &url), mcp_host::SpawnOptions::default())
        .await
        .unwrap();
    let first = mcp_host::call("hot", "echo", json!({"message": "before"}))
        .await
        .expect("first call should succeed");
    assert!(first.as_str().unwrap_or_default().starts_with("ok:"));

    // Reload — the underlying connection is dropped; the registration
    // and supervision state are reset.
    mcp_host::reload("hot").expect("reload should succeed");
    let snap = mcp_host::status().await;
    let entry = snap.iter().find(|s| s.name == "hot").expect("entry exists");
    assert_eq!(entry.restart_count, 0, "reload should reset restart_count");
    assert_eq!(entry.consecutive_failures, 0);

    // Calling again should transparently reconnect.
    let second = mcp_host::call("hot", "echo", json!({"message": "after"}))
        .await
        .expect("second call should succeed after reload");
    assert!(second.as_str().unwrap_or_default().starts_with("ok:"));

    mcp_host::stop("hot").ok();
    server.shutdown().await;
    reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_denied_by_allowlist_returns_typed_error() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    let server = spawn_fake_http_server(FakeServerBehavior::ModernSuccess).await;
    let url = format!("{}/mcp", server.base_url);

    // Install an allowlist that only admits a different server.
    mcp_host::set_allowlist(Some(std::sync::Arc::new(|name: &str, _tool| {
        if name == "permitted" {
            mcp_host::AllowlistDecision::Allow
        } else {
            mcp_host::AllowlistDecision::Deny {
                reason: format!("'{name}' not on tenant allowlist"),
            }
        }
    })));

    let err = mcp_host::spawn(spec_for("blocked", &url), mcp_host::SpawnOptions::default())
        .await
        .expect_err("spawn must reject the disallowed server");
    let msg = err.to_string();
    assert!(
        msg.contains("denied by allowlist") && msg.contains("blocked"),
        "expected denial reason, got: {msg}"
    );

    mcp_host::set_allowlist(None);
    server.shutdown().await;
    reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn repeat_call_with_cache_hint_is_served_from_response_cache() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    let server = spawn_fake_http_server(FakeServerBehavior::CacheHints).await;
    let url = format!("{}/mcp", server.base_url);

    mcp_host::spawn(
        spec_for("cacheable", &url),
        mcp_host::SpawnOptions::default(),
    )
    .await
    .unwrap();

    let first_stats = mcp_host::cache_stats();
    let first = mcp_host::call("cacheable", "echo", json!({"message": "hi"}))
        .await
        .expect("first call should reach the server");
    let post_first_stats = mcp_host::cache_stats();
    assert!(
        post_first_stats.misses > first_stats.misses,
        "first call must register as a cache miss; before={first_stats:?}, after={post_first_stats:?}"
    );

    let second = mcp_host::call("cacheable", "echo", json!({"message": "hi"}))
        .await
        .expect("second call must hit the cache");
    let post_second_stats = mcp_host::cache_stats();
    assert_eq!(
        first, second,
        "cached payload must equal the original response"
    );
    assert!(
        post_second_stats.hits > post_first_stats.hits,
        "repeat call with identical args must register a cache hit; before={post_first_stats:?}, after={post_second_stats:?}"
    );

    mcp_host::stop("cacheable").ok();
    server.shutdown().await;
    reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_spawn_does_not_eagerly_connect() {
    let _lock = global_test_lock().await;
    reset_thread_local_state();
    // We deliberately point at a non-existent URL: a lazy spawn must
    // not try to connect, so this should still succeed.
    let lazy_options = mcp_host::SpawnOptions {
        lazy: true,
        ..mcp_host::SpawnOptions::default()
    };
    let id = mcp_host::spawn(spec_for("lazy", "http://127.0.0.1:1/mcp"), lazy_options)
        .await
        .expect("lazy spawn must not perform an eager connect");
    assert_eq!(id, "lazy");

    let snap = mcp_host::status().await;
    let entry = snap.iter().find(|s| s.name == "lazy").expect("registered");
    assert!(entry.lazy);
    assert!(
        !entry.active,
        "lazy server must remain idle until first call"
    );

    mcp_host::stop("lazy").ok();
    reset_thread_local_state();
}
