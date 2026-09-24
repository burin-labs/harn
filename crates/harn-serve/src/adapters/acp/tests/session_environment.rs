//! The `session/new` environment policy is declared, never inferred.

use super::*;

/// harn#8566. A `session/new` that names no environment policy is refused,
/// and the refusal is usable on its own: it names the field and every value
/// the field accepts.
///
/// The defect this closes was not that the default was wrong. It was that a
/// client which declared nothing got `inherited` one day and `isolated` the
/// next, with no signal at either end. A refusal is what makes that state
/// unreachable, so the assertion to protect is that omission cannot open a
/// session at all, whatever this server's default would have been.
#[tokio::test(flavor = "current_thread")]
async fn session_new_refuses_an_omitted_environment_policy_and_names_its_values() {
    harn_vm::reset_thread_local_state();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;

    let refused = recv_json(&mut rx).await;
    assert!(
        refused["result"].is_null(),
        "an omitted policy must not open a session, got {refused}"
    );
    assert_eq!(refused["error"]["code"], serde_json::json!(-32602));
    let data = &refused["error"]["data"];
    assert_eq!(
        data["code"],
        serde_json::json!("environment_policy.missing")
    );
    assert_eq!(data["field"], serde_json::json!("environmentPolicy"));
    assert_eq!(
        data["accepted"],
        serde_json::json!(["inherited", "isolated", "granted"]),
        "the refusal must name every value the field accepts"
    );
    let message = refused["error"]["message"].as_str().expect("message");
    for kind in ["inherited", "isolated", "granted"] {
        assert!(
            message.contains(kind),
            "message must name {kind}: {message}"
        );
    }

    // The positive control, in the same test so the refusal cannot be a server
    // that refuses everything: a request that states the policy still opens.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    assert!(
        created["result"]["sessionId"].as_str().is_some(),
        "a stated policy must still open a session, got {created}"
    );
}
