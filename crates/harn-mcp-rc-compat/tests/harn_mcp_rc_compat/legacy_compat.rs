//! Legacy 2025-11-25 wire-compat regression for both directions.
//!
//! These tests guarantee that the RC enablement work has not silently
//! changed the byte-for-byte wire shape pre-RC clients see. Failures
//! here localize to the **legacy-compat** surface: either a Harn server
//! started leaking RC envelope fields into legacy responses, or the
//! fake legacy fixture itself drifted from spec.

use harn_mcp_rc_compat::fake_client::{legacy_request, post_rc};
use harn_mcp_rc_compat::fake_server::{spawn_fake_http_server, FakeServerBehavior};
use harn_mcp_rc_compat::fixtures::{load_named, WireFixtureKind};
use harn_mcp_rc_compat::generic_server_harness;
use serde_json::json;

#[tokio::test]
async fn legacy_initialize_against_generic_server_omits_rc_envelope_and_returns_session_id() {
    let server = generic_server_harness::spawn().await;
    let request = legacy_request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "clientInfo": {"name": "harn-rc-compat-legacy", "version": "0.1.0"},
            "capabilities": {}
        }),
    );
    let response = post_rc(&server.base_url, &request, &[]).await;
    assert_eq!(response.status, 200);

    let result = response
        .body
        .get("result")
        .expect("legacy initialize must return a result");
    assert_eq!(result["protocolVersion"], json!("2025-11-25"));
    assert!(
        result.get("resultType").is_none(),
        "legacy initialize must not include resultType, got {result}"
    );
    assert!(
        result.get("ttlMs").is_none(),
        "legacy initialize must not include ttlMs, got {result}"
    );
    assert!(
        response.echoed_session.is_some(),
        "legacy initialize must mint Mcp-Session-Id"
    );
    assert_eq!(
        response.echoed_protocol.as_deref(),
        Some("2025-11-25"),
        "legacy initialize must echo the stable protocol version"
    );
}

#[tokio::test]
async fn legacy_initialize_against_fake_legacy_server_round_trips() {
    let server = spawn_fake_http_server(FakeServerBehavior::Legacy202511).await;
    let url = format!("{}/mcp", server.base_url);
    let request = legacy_request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "clientInfo": {"name": "legacy", "version": "1"},
            "capabilities": {}
        }),
    );
    let response = post_rc(&url, &request, &[]).await;
    let result = response.body.get("result").expect("initialize result");
    assert_eq!(result["protocolVersion"], json!("2025-11-25"));
    assert!(result.get("resultType").is_none());
}

#[tokio::test]
async fn legacy_tools_list_against_fake_legacy_server_omits_envelope() {
    let server = spawn_fake_http_server(FakeServerBehavior::Legacy202511).await;
    let url = format!("{}/mcp", server.base_url);
    let request = legacy_request(2, "tools/list", json!({}));
    let response = post_rc(&url, &request, &[]).await;
    let result = response.body.get("result").expect("result");
    assert!(
        result.get("resultType").is_none(),
        "legacy tools/list result must not include resultType"
    );
    assert!(result["tools"].is_array());
}

#[tokio::test]
async fn fake_legacy_server_does_not_advertise_server_discover() {
    let server = spawn_fake_http_server(FakeServerBehavior::Legacy202511).await;
    let url = format!("{}/mcp", server.base_url);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {}
    });
    let response = post_rc(&url, &request, &[]).await;
    let error = response.body.get("error").expect("error response");
    assert_eq!(error["code"], json!(-32601));
}

#[tokio::test]
async fn published_legacy_fixture_round_trips_against_generic_server() {
    let server = generic_server_harness::spawn().await;
    let fixture = load_named("legacy_2025_11_25.json");
    assert_eq!(fixture.kind, WireFixtureKind::Exchange);
    // Legacy clients open a session on `initialize` and replay the
    // assigned `Mcp-Session-Id` on every subsequent request, so we
    // capture it here and thread it through.
    let mut session_header: Option<String> = None;
    for pair in fixture.documents.chunks(2) {
        if pair.len() != 2 {
            continue;
        }
        let request = &pair[0];
        let expected = &pair[1];
        let mut headers = Vec::new();
        if let Some(session) = &session_header {
            headers.push(("mcp-session-id", session.clone()));
        }
        let response = post_rc(&server.base_url, request, &headers).await;
        if let Some(session) = response.echoed_session.clone() {
            session_header = Some(session);
        }
        let result = response.body.get("result").expect("legacy result");
        let expected_result = expected.get("result").expect("expected result");
        for forbidden in ["resultType", "ttlMs", "cacheScope"] {
            assert!(
                result.get(forbidden).is_none(),
                "{} sprouted RC field {forbidden}: {result}",
                request["method"]
            );
            assert!(
                expected_result.get(forbidden).is_none(),
                "fixture {} unexpectedly carries {forbidden}",
                fixture.name
            );
        }
    }
}
