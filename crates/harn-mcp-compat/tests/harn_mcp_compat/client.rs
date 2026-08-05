//! Harness self-consistency tests for the fake stable servers.
//!
//! The fake servers in [`harn_mcp_compat::fake_server`] are the
//! reference fixtures that Harn's MCP client gets tested against (see
//! `crates/harn-vm/src/mcp/` for the in-process client wire tests).
//! This test file proves the fakes themselves agree with the published
//! JSON fixtures so the same wire shape is what downstream consumers
//! (downstream hosts, a cloud platform) replay in their own suites.
//!
//! Failures here localize to the **client-facing fake-server surface**:
//! either the fakes have drifted, or the published fixtures have.

use harn_mcp_compat::fake_client::{post_mcp, stable_headers, stable_meta};
use harn_mcp_compat::fake_server::{
    spawn_fake_http_server, spawn_fake_stdio_server, FakeServerBehavior,
};
use harn_mcp_compat::fixtures::load_named;
use serde_json::{json, Value as JsonValue};
use tokio::time::{timeout, Duration};

const RECV_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test]
async fn stable_success_fake_server_emits_envelope_and_cache_hint() {
    let server = spawn_fake_http_server(FakeServerBehavior::StableSuccess).await;
    let url = format!("{}/mcp", server.base_url);
    let discover = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": stable_meta("client.rs")},
    });
    let response = post_mcp(
        &url,
        &discover,
        &stable_headers("server/discover", &discover["params"]),
    )
    .await;
    assert_eq!(response.status, 200);
    let result = &response.body["result"];
    assert_eq!(result["resultType"], json!("complete"));
    assert!(result["supportedVersions"]
        .as_array()
        .expect("supportedVersions")
        .iter()
        .any(|v| v == &json!("2026-07-28")));

    let list = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {"_meta": stable_meta("client.rs")},
    });
    let list_resp = post_mcp(&url, &list, &stable_headers("tools/list", &list["params"])).await;
    let list_result = &list_resp.body["result"];
    assert_eq!(list_result["resultType"], json!("complete"));
    assert!(list_result.get("ttlMs").is_some());
}

#[tokio::test]
async fn header_mismatch_fake_server_validates_mcp_method_header() {
    let server = spawn_fake_http_server(FakeServerBehavior::HeaderMismatch).await;
    let url = format!("{}/mcp", server.base_url);

    // Agreeing header and body sail through to the normal handler.
    let list = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {"_meta": stable_meta("client.rs")},
    });
    let ok = post_mcp(&url, &list, &stable_headers("tools/list", &list["params"])).await;
    assert!(
        ok.body.get("result").is_some(),
        "matching headers must succeed; got {}",
        ok.body
    );

    // A spoofed `Mcp-Method` header is rejected with the stable `-32020`
    // shape, mirroring the strict generic server.
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "echo",
            "arguments": {"message": "spoofed"},
            "_meta": stable_meta("client.rs"),
        },
    });
    let bad = post_mcp(&url, &call, &stable_headers("tools/list", &call["params"])).await;
    assert_eq!(bad.body["error"]["code"], json!(-32020));
    assert_eq!(
        bad.body["error"]["data"]["headerValue"],
        json!("tools/list")
    );
    assert_eq!(bad.body["error"]["data"]["bodyMethod"], json!("tools/call"));
}

#[tokio::test]
async fn input_required_fake_server_returns_input_required_then_complete() {
    let server = spawn_fake_http_server(FakeServerBehavior::InputRequired).await;
    let url = format!("{}/mcp", server.base_url);

    let first = json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "needs_input",
            "arguments": {"prompt": "continue"},
            "_meta": stable_meta("client.rs"),
        }
    });
    let first_resp = post_mcp(
        &url,
        &first,
        &stable_headers("tools/call", &first["params"]),
    )
    .await;
    let first_result = &first_resp.body["result"];
    assert_eq!(first_result["resultType"], json!("input_required"));
    assert!(first_result["requestState"].is_string());
    assert!(first_result["inputRequests"].is_object());

    let second = json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "name": "needs_input",
            "arguments": {"prompt": "continue"},
            "requestState": first_result["requestState"].clone(),
            "inputResponses": {
                "approve": {"action": "accept", "content": {"approved": true}}
            },
            "_meta": stable_meta("client.rs"),
        }
    });
    let second_resp = post_mcp(
        &url,
        &second,
        &stable_headers("tools/call", &second["params"]),
    )
    .await;
    assert_eq!(second_resp.body["result"]["resultType"], json!("complete"));
}

#[tokio::test]
async fn cache_hints_fake_server_advertises_explicit_ttl_and_scope() {
    let server = spawn_fake_http_server(FakeServerBehavior::CacheHints).await;
    let url = format!("{}/mcp", server.base_url);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/list",
        "params": {"_meta": stable_meta("client.rs")},
    });
    let response = post_mcp(&url, &body, &stable_headers("tools/list", &body["params"])).await;
    let result = &response.body["result"];
    assert_eq!(result["ttlMs"], json!(300_000_u64));
    assert_eq!(result["cacheScope"], json!("public"));
}

#[tokio::test]
async fn recursive_defs_fake_server_advertises_recursive_schema() {
    let server = spawn_fake_http_server(FakeServerBehavior::RecursiveDefsTool).await;
    let url = format!("{}/mcp", server.base_url);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/list",
        "params": {"_meta": stable_meta("client.rs")},
    });
    let response = post_mcp(&url, &body, &stable_headers("tools/list", &body["params"])).await;
    let tool = &response.body["result"]["tools"][0];
    assert_eq!(tool["name"], json!("tree_summarize"));
    let schema = &tool["inputSchema"];
    assert_eq!(
        schema["$schema"],
        json!("https://json-schema.org/draft/2020-12/schema")
    );
    assert!(schema["$defs"]["Node"].is_object());
    assert_eq!(
        schema["$defs"]["Node"]["properties"]["children"]["items"]["$ref"],
        json!("#/$defs/Node")
    );
}

#[tokio::test]
async fn stdio_fake_server_round_trips_stable_request() {
    let mut handle = spawn_fake_stdio_server(FakeServerBehavior::StableSuccess);
    let discover = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": stable_meta("client.rs")},
    });
    handle
        .stdin_tx
        .send(format!("{discover}\n"))
        .expect("send line");
    let response_line = timeout(RECV_TIMEOUT, handle.stdout_rx.recv())
        .await
        .expect("recv timed out")
        .expect("server hung up");
    let response: JsonValue = serde_json::from_str(response_line.trim()).expect("parse json");
    let result = &response["result"];
    assert_eq!(result["resultType"], json!("complete"));
    assert!(result["supportedVersions"]
        .as_array()
        .expect("supportedVersions")
        .iter()
        .any(|v| v == &json!("2026-07-28")));
}

#[tokio::test]
async fn fake_server_published_fixtures_round_trip_through_fake_server() {
    // The published stable_success fixture has to remain executable as a
    // request/response sequence — otherwise downstream consumers replay
    // against drift the moment they upgrade.
    let fixture = load_named("stable_success.json");
    let server = spawn_fake_http_server(FakeServerBehavior::StableSuccess).await;
    let url = format!("{}/mcp", server.base_url);

    for pair in fixture.documents.chunks(2) {
        if pair.len() != 2 {
            continue;
        }
        let request = &pair[0];
        let expected = &pair[1];
        let method = request["method"].as_str().unwrap_or_default();
        let headers = stable_headers(method, &request["params"]);
        let response = post_mcp(&url, request, &headers).await;
        let result_type = response
            .body
            .get("result")
            .and_then(|r| r.get("resultType"))
            .cloned()
            .unwrap_or(JsonValue::Null);
        assert_eq!(
            result_type, expected["result"]["resultType"],
            "{method} resultType drift"
        );
    }
}
