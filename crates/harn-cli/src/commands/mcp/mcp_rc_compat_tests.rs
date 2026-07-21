//! Orchestrator-server RC compatibility tests.
//!
//! Drives the shared `harn-mcp-rc-compat` fixtures + fake-client helpers
//! against the in-process `McpOrchestratorService`. Failures here
//! localize to the **orchestrator-server** surface — see
//! `crates/harn-mcp-rc-compat/tests/generic_server.rs` for the parallel
//! coverage on the generic `harn-serve` MCP wrapper.
//!
//! The in-process drive avoids the 30–40 s cold-start the subprocess
//! tests pay so this suite stays in the fast `make test` path.

use super::*;
use std::fs;
use std::path::Path;

use harn_mcp_rc_compat::fake_client::{legacy_request, rc_meta, rc_request};
use harn_mcp_rc_compat::fixtures::{load_named, WireFixtureKind};
use serde_json::json;
use tempfile::TempDir;

use crate::tests::common::harn_state_lock::lock_harn_state_async;

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_minimal_fixture(temp: &TempDir) {
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "rc-compat-fixture"

[exports]
handlers = "lib.harn"
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
pub fn ping() -> string {
  return "pong"
}
"#,
    );
}

fn fixture_args(temp: &TempDir) -> McpServeArgs {
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    McpServeArgs {
        local: OrchestratorLocalArgs {
            config: temp.path().join("harn.toml"),
            state_dir,
        },
        transport: McpServeTransport::Stdio,
        bind: "127.0.0.1:0".parse().unwrap(),
        path: "/mcp".to_string(),
        sse_path: "/sse".to_string(),
        messages_path: "/messages".to_string(),
    }
}

async fn fresh_service() -> (McpOrchestratorService, TempDir) {
    let temp = TempDir::new().unwrap();
    write_minimal_fixture(&temp);
    let args = fixture_args(&temp);
    let service = McpOrchestratorService::new_local(args.local).unwrap();
    (service, temp)
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_modern_tools_list_carries_rc_envelope_and_cache_hint() {
    let _guard = lock_harn_state_async().await;
    let (service, _temp) = fresh_service().await;
    let mut session = ConnectionState::default();
    let request = rc_request(1, "tools/list", json!({}), "harn-rc-compat-client");
    let response = service.handle_request(&mut session, request).await;
    let result = response.get("result").expect("tools/list result");
    assert_eq!(result["resultType"], json!("complete"));
    assert!(
        result.get("ttlMs").is_some(),
        "modern tools/list must include cache hint"
    );
    assert_eq!(result["cacheScope"], json!("private"));
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_modern_non_list_methods_carry_result_type_envelope() {
    let _guard = lock_harn_state_async().await;
    let (service, _temp) = fresh_service().await;
    let mut session = ConnectionState::default();
    for method in ["ping", "tasks/list"] {
        let request = rc_request(1, method, json!({}), "harn-rc-compat-client");
        let response = service.handle_request(&mut session, request).await;
        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("{method} should return result, got {response:?}"));
        assert_eq!(
            result["resultType"],
            json!("complete"),
            "{method} modern response must carry RC envelope"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_unsupported_meta_version_returns_minus_32004() {
    let _guard = lock_harn_state_async().await;
    let (service, _temp) = fresh_service().await;
    let mut session = ConnectionState::default();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/list",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2099-01-01",
                "io.modelcontextprotocol/clientInfo": {"name": "fuzz", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    let response = service.handle_request(&mut session, request).await;
    let error = response.get("error").expect("error response");
    assert_eq!(error["code"], json!(-32004));
    let supported = error["data"]["supported"]
        .as_array()
        .expect("supported array");
    assert!(supported.iter().any(|v| v == &json!("DRAFT-2026-v1")));
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_server_discover_returns_capabilities_and_skips_initialize() {
    let _guard = lock_harn_state_async().await;
    let (service, _temp) = fresh_service().await;
    let mut session = ConnectionState::default();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": rc_meta("harn-rc-compat-client")}
    });
    let response = service.handle_request(&mut session, request).await;
    let result = response.get("result").expect("discover result");
    assert_eq!(result["resultType"], json!("complete"));
    let supported = result["supportedVersions"]
        .as_array()
        .expect("supportedVersions array");
    assert!(supported.iter().any(|v| v == &json!("DRAFT-2026-v1")));
    assert!(supported.iter().any(|v| v == &json!("2025-11-25")));
    assert!(session.initialized);
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_legacy_initialize_omits_rc_envelope() {
    let _guard = lock_harn_state_async().await;
    let (service, _temp) = fresh_service().await;
    let mut session = ConnectionState::default();
    let request = legacy_request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "legacy", "version": "1"}
        }),
    );
    let response = service.handle_request(&mut session, request).await;
    let result = response.get("result").expect("legacy initialize result");
    assert_eq!(result["protocolVersion"], json!("2025-11-25"));
    assert!(
        result.get("resultType").is_none(),
        "legacy initialize must not include RC envelope: {result}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_replays_published_modern_success_fixture() {
    let _guard = lock_harn_state_async().await;
    let (service, _temp) = fresh_service().await;
    let fixture = load_named("modern_success.json");
    assert_eq!(fixture.kind, WireFixtureKind::Exchange);
    let mut session = ConnectionState::default();
    for pair in fixture.documents.chunks(2) {
        if pair.len() != 2 {
            continue;
        }
        let request = pair[0].clone();
        let expected = &pair[1];
        let method = request["method"].as_str().unwrap_or_default().to_string();
        // The orchestrator does not host a `tools/call` for the harness
        // fixture's `echo` tool; skip the call step (the response shape
        // is exercised against the generic server instead). Discover +
        // list still validate the orchestrator's RC envelope/cache hint
        // semantics against the published fixture.
        if method == "tools/call" {
            continue;
        }
        let response = service.handle_request(&mut session, request).await;
        let result = response.get("result").expect("result");
        let expected_result = expected.get("result").expect("expected result");
        assert_eq!(
            result["resultType"], expected_result["resultType"],
            "{method} resultType drift against published fixture"
        );
    }
}
