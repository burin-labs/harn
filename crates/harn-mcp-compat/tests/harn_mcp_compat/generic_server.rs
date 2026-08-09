//! Stable MCP clients driven against the generic `harn-serve::McpServer`.
//!
//! Failures in this file localize to the **generic-server** surface:
//! they tell you the bundled MCP wrapper around an arbitrary `.harn`
//! script regressed stable behavior. The official SDK test is the canonical
//! interoperability path; exact wire tests cover error cases. The
//! orchestrator's handling is covered in `harn-cli`'s `mcp_compat_tests`.

use harn_mcp_compat::fake_client::{post_mcp, stable_headers, stable_meta, stable_request};
use harn_mcp_compat::fixtures::{load_named, WireFixtureKind};
use harn_mcp_compat::generic_server_harness;
use rmcp::model::{ClientCapabilities, ClientInfo, Implementation, ProtocolVersion};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::json;

#[tokio::test]
async fn official_sdk_discovers_and_calls_generic_server() {
    let server = generic_server_harness::spawn().await;
    let transport = StreamableHttpClientTransport::from_uri(server.base_url.clone());
    let client_info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("harn-rmcp-interop", env!("CARGO_PKG_VERSION")),
    )
    .with_protocol_version(ProtocolVersion::V_2026_07_28);
    let mut client = client_info
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("official SDK should discover Harn's stable server");

    assert_eq!(
        client
            .peer_info()
            .expect("negotiated server info")
            .protocol_version,
        ProtocolVersion::V_2026_07_28
    );
    let tools = client
        .list_all_tools()
        .await
        .expect("official SDK should list Harn tools");
    assert!(tools.iter().any(|tool| tool.name == "echo"));
    client.close().await.expect("close official SDK client");
}

#[tokio::test]
async fn stable_tools_list_returns_stable_envelope_and_cache_hint() {
    let server = generic_server_harness::spawn().await;
    let body = stable_request(1, "tools/list", json!({}), "harn-mcp-compat-client");
    let headers = stable_headers("tools/list", &body["params"]);
    let response = post_mcp(&server.base_url, &body, &headers).await;

    assert_eq!(response.status, 200, "stable tools/list must return 200");
    assert_eq!(
        response.echoed_protocol.as_deref(),
        Some("2026-07-28"),
        "server must echo the negotiated protocol"
    );
    assert!(
        response.echoed_session.is_none(),
        "stable responses must not mint a session id; got {:?}",
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
async fn stable_tools_call_returns_stable_envelope() {
    let server = generic_server_harness::spawn().await;
    let body = stable_request(
        2,
        "tools/call",
        json!({"name": "echo", "arguments": {"message": "hi"}}),
        "harn-mcp-compat-client",
    );
    let headers = stable_headers("tools/call", &body["params"]);
    let response = post_mcp(&server.base_url, &body, &headers).await;

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
async fn server_discover_returns_request_metadata_versions() {
    let server = generic_server_harness::spawn().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": stable_meta("harn-mcp-compat-client")},
    });
    let headers = stable_headers("server/discover", &body["params"]);
    let response = post_mcp(&server.base_url, &body, &headers).await;

    assert_eq!(response.status, 200);
    let result = &response.body["result"];
    assert_eq!(result["resultType"], json!("complete"));
    let supported = result["supportedVersions"]
        .as_array()
        .expect("supported versions array");
    assert_eq!(supported, &[json!("2026-07-28")]);
}

#[tokio::test]
async fn unsupported_version_request_returns_minus_32022() {
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
    let response = post_mcp(&server.base_url, &body, &[]).await;
    let error = response.body.get("error").expect("error response");
    assert_eq!(error["code"], json!(-32022));
    let supported = error["data"]["supported"]
        .as_array()
        .expect("supported array");
    assert!(supported.iter().any(|v| v == &json!("2026-07-28")));
}

#[tokio::test]
async fn header_method_mismatch_returns_minus_32020() {
    let server = generic_server_harness::spawn().await;
    let body = stable_request(
        42,
        "tools/call",
        json!({"name": "echo", "arguments": {"message": "spoofed"}}),
        "harn-mcp-compat-client",
    );
    // Deliberately mismatch the Mcp-Method header against the body.
    let bad_headers = vec![
        ("mcp-protocol-version", "2026-07-28".to_string()),
        ("mcp-method", "tools/list".to_string()),
    ];
    let response = post_mcp(&server.base_url, &body, &bad_headers).await;
    let error = response.body.get("error").expect("error response");
    assert_eq!(error["code"], json!(-32020));
    assert_eq!(error["data"]["headerValue"], json!("tools/list"));
    assert_eq!(error["data"]["bodyMethod"], json!("tools/call"));
}

#[tokio::test]
async fn published_stable_success_fixture_matches_server_responses() {
    let server = generic_server_harness::spawn().await;
    let fixture = load_named("stable_success.json");
    assert_eq!(fixture.kind, WireFixtureKind::Exchange);

    // Replay each (request, response) pair. The published fixture lets
    // downstream consumers (a host, a cloud platform) drive identical
    // sequences and assert the same envelope/cache shape Harn does.
    for pair in fixture.documents.chunks(2) {
        if pair.len() != 2 {
            continue;
        }
        let request = &pair[0];
        let expected = &pair[1];
        let method = request["method"].as_str().unwrap_or_default();
        if method == "server/discover" || method.starts_with("tools/") {
            let headers = stable_headers(method, &request["params"]);
            let response = post_mcp(&server.base_url, request, &headers).await;
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
async fn stable_request_without_session_id_succeeds_without_minting_one() {
    let server = generic_server_harness::spawn().await;
    let body = stable_request(1, "tools/list", json!({}), "harn-mcp-compat-client");
    let headers = stable_headers("tools/list", &body["params"]);
    let response = post_mcp(&server.base_url, &body, &headers).await;
    assert_eq!(response.status, 200);
    assert!(
        response.echoed_session.is_none(),
        "stable responses must stay session-less; got {:?}",
        response.echoed_session
    );
}
