//! A served session must be able to run its own session bookkeeping in the
//! default mode.
//!
//! `session/new` starts every session in `ask` (`modes.rs::DEFAULT_MODE_ID`),
//! whose autonomy tier (`ActWithApproval`) installs a `read_only` side-effect
//! ceiling for the turn and no capability restriction at all — so on a served
//! turn the coarse ladder is the only gate. The agent loop opens the session
//! its model calls are recorded in via `harness.agent.open`, which declares
//! `state:mutate (agent-sessions)`. The ladder ranks that `workspace_write`,
//! so every served turn was rejected before its first model call:
//!
//! ```text
//! harness.agent.open exceeds the active effect ceiling: state:mutate (agent-sessions)
//! ```
//!
//! `harn run` never saw it — it installs no ceiling at all, so the run path
//! granted what the served path withheld. The fix is one typed declaration on
//! the contract — `runtime_infrastructure`, meaning the agent runtime
//! performed the operation on its own behalf, which exempts its effects from
//! the tool-invasiveness ladder and nothing else — not a served-only
//! exception. See `crates/harn-vm/src/orchestration/tests/side_effect_ceiling.rs`
//! for the registry-wide guard and the in-process negative controls.
//!
//! ## Why this drives no model call
//!
//! The obvious end-to-end shape — arm the LLM fixture, then call the model —
//! cannot be written in this mode. `harness.llm.mock_clear` and
//! `mock_enqueue` declare `state:mutate/write (llm-fixture)`, which the same
//! ladder also ranks `workspace_write`, so the fixture is itself above the
//! `ask` ceiling. Every other mock-LLM ACP test arms it from **code** mode
//! (`start_acp_code_session_with_config`) for exactly this reason.
//!
//! Marking the fixture `runtime_infrastructure` would be false — a test
//! fixture is not the agent runtime acting on its own behalf — so this file
//! proves the control plane instead, and a served turn that reaches a real
//! model call in the default mode stays uncovered. That gap is a property of
//! the fixture, not of the fix.

use super::*;

fn answer_host_capabilities(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    message: &serde_json::Value,
) -> bool {
    if message["method"] != "host/capabilities" {
        return false;
    }
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"].clone(),
            "result": {},
        }))
        .expect("send host capabilities response");
    true
}

/// Send one `session/prompt` and return its response, answering the host
/// capability handshake along the way.
async fn prompt(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: u64,
    source: &str,
) -> serde_json::Value {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": source}],
            },
        }))
        .expect("send session/prompt");

    for _ in 0..64 {
        let message = recv_json(response_rx).await;
        if answer_host_capabilities(request_tx, &message) {
            continue;
        }
        if message["id"] == id {
            return message;
        }
    }
    panic!("timed out waiting for session/prompt response {id}");
}

/// The falsifier for the served-path blocker, driven over the real ACP wire in
/// the real default mode with no `session/set_mode`, and its control.
///
/// Both halves run against the **same live session**, because each is worth
/// nothing alone:
///
///   * The admit half alone would pass if this harness simply installed no
///     ceiling — the failure mode the bug itself came from, where `harn run`
///     looked fine because it had no ceiling to violate.
///   * The refuse half alone would pass if everything were rejected.
///
/// Disarm by deleting `runtime_infrastructure` from `harness.agent.open` in
/// `crates/harn-capability-contracts/src/ai.rs`: the admit half then fails
/// with the ceiling rejection quoted in the module docs, while the refuse half
/// keeps passing.
#[tokio::test(flavor = "current_thread")]
async fn default_mode_admits_the_session_control_plane_and_still_refuses_durable_state() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // No session/set_mode: this is the `ask` default a client gets.
            let (request_tx, mut response_rx, _server, session_id) =
                start_acp_channel_session().await;

            // Three of the 27 marked control-plane methods, in the order the
            // agent loop itself uses them. Each declares a `state` write that
            // the ladder ranks above `read_only`.
            let control_plane = concat!(
                "const s = harness.agent.open()\n",
                "harness.agent.set_scratchpad(s, {probe: \"reached\"})\n",
                "harness.agent.close(s)\n",
                "harness.stdio.println(\"control-plane-ok\")\n",
            );
            let admitted =
                prompt(&request_tx, &mut response_rx, &session_id, 2, control_plane).await;
            assert!(
                admitted["error"].is_null(),
                "the agent loop's own session control plane must survive the ceiling \
                 every non-`code` session mode installs: {}",
                admitted["error"]
            );
            assert_eq!(
                admitted["result"]["stopReason"], "end_turn",
                "default-mode control-plane turn should complete; got {}",
                admitted["result"]
            );

            // The control. `harness.agent.state_write` is the sharpest possible
            // sibling: same capability handle, same `state` effect kind, same
            // `write` access, same ceiling, same session. It carries no marker
            // because its first argument is an fs-backed durable handle rather
            // than a session id. If this is ever admitted, the ceiling has
            // stopped meaning anything and the turn above proves nothing.
            let durable = prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                3,
                "harness.agent.state_write(\"durable-handle\", \"k\", \"v\")\n",
            )
            .await;
            let rejection = format!(
                "{}{}",
                durable["error"]["message"].as_str().unwrap_or_default(),
                durable["result"]["stopReason"].as_str().unwrap_or_default()
            );
            assert!(
                rejection.contains("exceeds the active effect ceiling"),
                "`runtime_infrastructure` must not become a durable-state write grant, \
                 and this session must really be carrying the ceiling; got error={} \
                 result={}",
                durable["error"],
                durable["result"]
            );

            drop(request_tx);
        })
        .await;
}
