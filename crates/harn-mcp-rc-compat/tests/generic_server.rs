//! Fake RC MCP client driven against the generic `harn-serve::McpServer`.
//!
//! Failures in this file localize to the **generic-server** surface:
//! they tell you the bundled MCP wrapper around an arbitrary `.harn`
//! script regressed an RC behavior. The orchestrator's RC handling is
//! covered in `harn-cli`'s `mcp_rc_compat_tests`.

use harn_mcp_rc_compat::fake_client::{legacy_request, post_rc, rc_headers, rc_meta, rc_request};
use harn_mcp_rc_compat::fixtures::{load_named, WireFixtureKind};
use harn_mcp_rc_compat::generic_server_harness;
use serde_json::json;

#[tokio::test]
async fn modern_tools_list_returns_rc_envelope_and_cache_hint() {
    let server = generic_server_harness::spawn().await;
    let body = rc_request(1, "tools/list", json!({}), "harn-rc-compat-client");
    let headers = rc_headers("tools/list", &body["params"]);
    let response = post_rc(&server.base_url, &body, &headers).await;

    assert_eq!(response.status, 200, "modern tools/list must return 200");
    assert_eq!(
        response.echoed_protocol.as_deref(),
        Some("DRAFT-2026-v1"),
        "server must echo the negotiated protocol"
    );
    assert!(
        response.echoed_session.is_none(),
        "modern responses must not mint a session id; got {:?}",
        response.echoed_session
    );

    let result = response
        .body
        .get("result")
        .expect("tools/list result present");
    assert_eq!(result["resultType"], json!("complete"));
    assert!(
        result.get("ttlMs").is_some(),
        "tools/list must carry a cache TTL hint, got {result}"
    );
    assert_eq!(result["cacheScope"], json!("private"));
    let tools = result["tools"].as_array().expect("tools array");
    assert!(tools.iter().any(|tool| tool["name"] == json!("echo")));
}

#[tokio::test]
async fn modern_tools_call_returns_rc_envelope() {
    let server = generic_server_harness::spawn().await;
    let body = rc_request(
        2,
        "tools/call",
        json!({"name": "echo", "arguments": {"message": "hi"}}),
        "harn-rc-compat-client",
    );
    let headers = rc_headers("tools/call", &body["params"]);
    let response = post_rc(&server.base_url, &body, &headers).await;

    assert_eq!(response.status, 200);
    let result = response
        .body
        .get("result")
        .expect("tools/call result present");
    assert_eq!(result["resultType"], json!("complete"));
    assert_eq!(result["isError"], json!(false));
    assert!(
        result["content"][0]["text"]
            .as_str()
            .map(|t| t.contains("hi"))
            .unwrap_or(false),
        "tool output must echo the input message, got {result}"
    );
}

#[tokio::test]
async fn server_discover_returns_both_supported_versions() {
    let server = generic_server_harness::spawn().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": rc_meta("harn-rc-compat-client")},
    });
    let headers = rc_headers("server/discover", &body["params"]);
    let response = post_rc(&server.base_url, &body, &headers).await;

    assert_eq!(response.status, 200);
    let result = &response.body["result"];
    assert_eq!(result["resultType"], json!("complete"));
    let supported = result["supportedVersions"]
        .as_array()
        .expect("supportedVersions array");
    assert!(supported.iter().any(|v| v == &json!("DRAFT-2026-v1")));
    assert!(supported.iter().any(|v| v == &json!("2025-11-25")));
}

#[tokio::test]
async fn unsupported_version_request_returns_minus_32004() {
    let server = generic_server_harness::spawn().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/list",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2099-01-01",
                "io.modelcontextprotocol/clientInfo": {"name": "fuzz", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    let response = post_rc(&server.base_url, &body, &[]).await;
    let error = response.body.get("error").expect("error response");
    assert_eq!(error["code"], json!(-32004));
    let supported = error["data"]["supported"]
        .as_array()
        .expect("supported array");
    assert!(supported.iter().any(|v| v == &json!("DRAFT-2026-v1")));
}

#[tokio::test]
async fn header_method_mismatch_returns_minus_32600() {
    let server = generic_server_harness::spawn().await;
    let body = rc_request(
        42,
        "tools/call",
        json!({"name": "echo", "arguments": {"message": "spoofed"}}),
        "harn-rc-compat-client",
    );
    // Deliberately mismatch the Mcp-Method header against the body.
    let bad_headers = vec![
        ("mcp-protocol-version", "DRAFT-2026-v1".to_string()),
        ("mcp-method", "tools/list".to_string()),
    ];
    let response = post_rc(&server.base_url, &body, &bad_headers).await;
    let error = response.body.get("error").expect("error response");
    assert_eq!(error["code"], json!(-32600));
    assert_eq!(error["data"]["headerValue"], json!("tools/list"));
    assert_eq!(error["data"]["bodyMethod"], json!("tools/call"));
}

#[tokio::test]
async fn legacy_initialize_omits_rc_envelope() {
    let server = generic_server_harness::spawn().await;
    let init = legacy_request(
        1,
        "initialize",
        json!({"protocolVersion": "2025-11-25", "clientInfo": {"name": "legacy", "version": "1"}}),
    );
    let response = post_rc(&server.base_url, &init, &[]).await;
    assert_eq!(response.status, 200);
    let result = response.body.get("result").expect("initialize result");
    assert_eq!(result["protocolVersion"], json!("2025-11-25"));
    assert!(
        result.get("resultType").is_none(),
        "legacy initialize must not carry resultType; got {result}"
    );
    assert!(
        result.get("ttlMs").is_none(),
        "legacy initialize must not carry cache hints; got {result}"
    );
}

#[tokio::test]
async fn published_modern_success_fixture_matches_server_responses() {
    let server = generic_server_harness::spawn().await;
    let fixture = load_named("modern_success.json");
    assert_eq!(fixture.kind, WireFixtureKind::Exchange);

    // Replay each (request, response) pair. The published fixture lets
    // downstream consumers (Burin Code, harn-cloud) drive identical
    // sequences and assert the same envelope/cache shape Harn does.
    for pair in fixture.documents.chunks(2) {
        if pair.len() != 2 {
            continue;
        }
        let request = &pair[0];
        let expected = &pair[1];
        let method = request["method"].as_str().unwrap_or_default();
        if method == "server/discover" || method.starts_with("tools/") {
            let headers = rc_headers(method, &request["params"]);
            let response = post_rc(&server.base_url, request, &headers).await;
            let result = response.body.get("result").expect("result present");
            let expected_result = expected.get("result").expect("expected result");
            assert_eq!(
                result["resultType"], expected_result["resultType"],
                "resultType drift on {method}: got {result}, expected {expected_result}"
            );
        }
    }
}

#[tokio::test]
async fn modern_request_without_session_id_succeeds_without_minting_one() {
    let server = generic_server_harness::spawn().await;
    let body = rc_request(1, "tools/list", json!({}), "harn-rc-compat-client");
    let headers = rc_headers("tools/list", &body["params"]);
    let response = post_rc(&server.base_url, &body, &headers).await;
    assert_eq!(response.status, 200);
    assert!(
        response.echoed_session.is_none(),
        "modern responses must stay session-less; got {:?}",
        response.echoed_session
    );
}
