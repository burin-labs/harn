//! Cumulative-deadline behavior for the stdio MCP client (harn#4390).
//!
//! Kept in its own module so `tests.rs` — already an oversized,
//! ratchet-grandfathered file — does not grow. Reuses the stdio test-server
//! harness from the sibling `tests` module.

use super::tests::connect_stdio_test_script;
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn stdio_call_is_bounded_by_one_cumulative_deadline_under_notification_flood() {
    // A server that answers the handshake and then streams progress
    // notifications forever — never the response — used to reset the per-read
    // timeout on every line and block the agent turn indefinitely (harn#4390).
    // With a single cumulative deadline the call must return a timeout error
    // in ~budget, regardless of how many notifications arrive.
    let script = r#"
import json, sys
discover = json.loads(sys.stdin.readline())
assert discover["method"] == "server/discover"
print(json.dumps({
"jsonrpc": "2.0",
"id": discover["id"],
"result": {
    "resultType": "complete",
    "supportedVersions": ["DRAFT-2026-v1"],
    "capabilities": {"tools": {}},
    "serverInfo": {"name": "flooder", "version": "1.0.0"}
}
}), flush=True)
# Read the tool call and flood progress notifications, never answering it.
json.loads(sys.stdin.readline())
n = 0
while True:
    n += 1
    sys.stdout.write(json.dumps({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"progressToken": "t", "progress": n},
    }) + "\n")
    sys.stdout.flush()
"#;
    let handle = connect_stdio_test_script(
        script,
        McpProtocolMode::Modern,
        DRAFT_PROTOCOL_VERSION.to_string(),
    )
    .await;

    handle
        .set_stdio_response_deadline_for_test(std::time::Duration::from_millis(300))
        .await;

    // The outer guard is generous: with the fix the call returns in ~300ms;
    // without it (per-read reset) the flood keeps the loop alive and this
    // trips instead of hanging the whole suite, so a regression fails loudly.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handle.call(
            "tools/call",
            serde_json::json!({"name": "spin", "arguments": {}}),
        ),
    )
    .await
    .expect("cumulative deadline must bound the call; it ran past 10s");

    let error = result.expect_err("a server that never answers must produce an error");
    assert!(
        format!("{error:?}").contains("did not respond"),
        "expected a response-timeout error, got {error:?}"
    );
}
