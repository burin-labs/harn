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
//! the contract: `runtime_control_plane` classifies operations that mutate
//! Harn-owned session state rather than the user's workspace or an external
//! system. It exempts only the orthogonal user-world side-effect ladder; the
//! capability gate, effect receipts, and lineage still apply. See
//! `crates/harn-vm/src/orchestration/tests/side_effect_ceiling.rs` for the
//! registry-wide guard and the in-process negative controls.
//!
//! This test arms the mock provider outside the governed Harn source, then
//! drives `agent_loop` through the real default-mode ACP path. The task itself
//! asserts that one model call was consumed before the terminal response.

use super::*;

struct MockModeGuard;

impl Drop for MockModeGuard {
    fn drop(&mut self) {
        harn_vm::llm::clear_cli_llm_mock_mode();
    }
}

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

    let mut seen_methods = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let line = response_rx
                .recv()
                .await
                .expect("ACP response channel closed");
            let message: serde_json::Value = serde_json::from_str(&line).expect("ACP JSON line");
            if answer_host_capabilities(request_tx, &message) {
                seen_methods.push("host/capabilities".to_string());
                continue;
            }
            if message["id"] == id {
                return message;
            }
            seen_methods.push(
                message["method"]
                    .as_str()
                    .unwrap_or("<response-with-another-id>")
                    .to_string(),
            );
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("timed out waiting for session/prompt response {id}; saw {seen_methods:?}")
    })
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
/// Disarm by deleting `runtime_control_plane` from `harness.agent.open` in
/// `crates/harn-capability-contracts/src/ai.rs`: the admit half then fails
/// with the ceiling rejection quoted in the module docs, while the refuse half
/// keeps passing.
#[tokio::test(flavor = "current_thread")]
async fn default_mode_reaches_a_model_turn_and_still_refuses_a_workspace_write() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock = harn_vm::llm::parse_llm_mock_value(&serde_json::json!({
                "text": "all done",
                "model": "served-proof",
                "provider": "mock",
            }))
            .expect("mock fixture");
            harn_vm::llm::install_cli_llm_mocks(vec![mock]);
            let _mock_mode = MockModeGuard;

            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline = dir.path().join("served-agent-turn.harn");
            std::fs::write(
                &pipeline,
                r#"import { agent_loop } from "std/agent/loop"
pipeline default(harness: Harness) {
  if prompt == "runtime-control" {
    agent_loop(harness, prompt, nil, {provider: "mock", model: "served-proof"})
    assert(
      len(harness.llm.mock_calls()) == 1,
      "the default served task must consume exactly one model call",
    )
    harness.stdio.println("control-plane-ok")
  } else {
    harness.fs.write_text("ceiling-probe.txt", "must-not-write")
  }
}
"#,
            )
            .expect("write served pipeline");

            // No session/set_mode: this is the `ask` default a client gets.
            let (request_tx, mut response_rx, _server, session_id) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            let admitted = prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                2,
                "runtime-control",
            )
            .await;
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

            // The control: an unmarked write, on the same live session, that
            // the ladder must still refuse. If this is ever admitted, the
            // ceiling has stopped meaning anything and the turn above proves
            // nothing.
            //
            let durable = prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                3,
                "workspace-write",
            )
            .await;
            let rejection = format!(
                "{}{}",
                durable["error"]["message"].as_str().unwrap_or_default(),
                durable["result"]["stopReason"].as_str().unwrap_or_default()
            );
            assert!(
                rejection.contains("exceeds the active effect ceiling"),
                "this session must really be carrying the `read_only` ceiling, or the \
                 admitted control-plane turn above proves nothing; got error={} \
                 result={}",
                durable["error"],
                durable["result"]
            );
            assert!(
                !dir.path().join("ceiling-probe.txt").exists(),
                "the rejected workspace write reached the filesystem"
            );

            drop(request_tx);
        })
        .await;
}
