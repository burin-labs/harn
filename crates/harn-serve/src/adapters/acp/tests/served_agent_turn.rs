//! A served session must be able to run an agent turn in its default mode.
//!
//! `session/new` starts every session in `ask` (`modes.rs::DEFAULT_MODE_ID`),
//! whose autonomy tier installs a `read_only` side-effect ceiling for the
//! turn. The agent loop opens the session its model calls are recorded in via
//! `harness.agent.open`, which declares `state:mutate (agent-sessions)`. Judged
//! on the coarse side-effect ladder that ranks `workspace_write`, so every
//! served turn was rejected before the first model call:
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
//! exception; see
//! `crates/harn-vm/src/orchestration/tests/side_effect_ceiling.rs` for the
//! registry-wide guard and the exact-text negative control.
//!
//! Because no turn ever reached a cancellable state, `session/cancel` was
//! advertised on a served session but unprovable. The second test here closes
//! that: it cancels a live default-mode agent loop and reads the terminal.

use super::*;

/// Deterministic liveness signal for a default-mode turn. `harness.fs.append`
/// — what the code-mode cancel tests count ticks with — is itself above the
/// `read_only` ceiling, so progress is read off the ACP presentation stream
/// instead of the filesystem.
fn is_agent_message_chunk(message: &serde_json::Value) -> bool {
    message["method"] == "session/update"
        && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
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

/// The narrow falsifier: the exact call the served path died on, driven over
/// the real ACP wire in the real default mode, with no `session/set_mode`.
///
/// Disarm by deleting `runtime_infrastructure` from `harness.agent.open` in
/// `crates/harn-capability-contracts/src/ai.rs`; this test then fails with the
/// ceiling rejection quoted in the module docs.
#[tokio::test(flavor = "current_thread")]
async fn default_mode_prompt_opens_an_agent_session_and_calls_the_model() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, _server, session_id) =
                start_acp_channel_session().await;

            // No session/set_mode: this is the `ask` default a client gets.
            let prompt_source = concat!(
                "const s = harness.agent.open()\n",
                "harness.llm.mock_clear()\n",
                "harness.llm.mock_enqueue({text: \"served-turn-ok\", input_tokens: 1, \
                 output_tokens: 1, model: \"mock\", provider: \"mock\"})\n",
                "const r = harness.llm.call(\"hello\", nil, {provider: \"mock\", \
                 model: \"mock\", session_id: s})\n",
                "harness.agent.close(s)\n",
                "harness.runtime.emit_response({text: r.text})\n",
            );
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt_source}],
                    },
                }))
                .expect("send session/prompt");

            let mut streamed = String::new();
            let mut result = serde_json::Value::Null;
            for _ in 0..64 {
                let message = recv_json(&mut response_rx).await;
                if answer_host_capabilities(&request_tx, &message) {
                    continue;
                }
                if is_agent_message_chunk(&message) {
                    if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                        streamed.push_str(text);
                    }
                    continue;
                }
                if message["id"] == 2 {
                    assert!(
                        message["error"].is_null(),
                        "a default-mode served turn must not be rejected by the effect \
                         ceiling: {}",
                        message["error"]
                    );
                    result = message["result"].clone();
                    break;
                }
            }

            assert_eq!(
                result["stopReason"], "end_turn",
                "default-mode served turn should complete; got {result}"
            );
            // Not just "no error": the model call has to have actually happened
            // inside the opened session, or a turn that silently did nothing
            // would pass this test.
            assert!(
                streamed.contains("served-turn-ok"),
                "the served turn must reach the model inside the session it opened; \
                 streamed {streamed:?}"
            );

            drop(request_tx);
        })
        .await;
}

/// The product claim `control_capabilities` already advertises: a served turn
/// can be stopped. Unprovable until a served turn could start at all.
#[tokio::test(flavor = "current_thread")]
async fn default_mode_agent_loop_is_cancellable_mid_turn() {
    const MAX_ITERATIONS: usize = 30;
    const TICK_SLEEP_MS: u64 = 120;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("served-cancel-loop.harn");

            let mut mocks = String::new();
            for index in 0..MAX_ITERATIONS {
                mocks.push_str(&format!(
                    "  harness.llm.mock_enqueue({{text: \"tick-{index}\", tool_calls: \
                     [{{id: \"c{index}\", name: \"wait\", arguments: {{n: {index}}}}}]}})\n"
                ));
            }

            let source = format!(
                r#"import {{ agent_loop }} from "std/agent/loop"

fn wait_tools(harness: Harness) {{
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "wait",
    "Wait one tick",
    {{
      handler: {{ args ->
        harness.clock.sleep_ms({TICK_SLEEP_MS})
        "waited"
      }},
      parameters: {{n: {{type: "number", description: "Tick index"}}}},
      returns: {{type: "string"}},
      annotations: {{kind: "read"}},
    }},
  )
  return tools
}}

pipeline default(harness: Harness, task: unknown) {{
  harness.llm.mock_clear()
{mocks}
  const result = agent_loop(harness, "wait until told to stop", nil, {{
    provider: "mock",
    tools: wait_tools(harness),
    tool_format: "native",
    loop_until_done: true,
    max_iterations: {MAX_ITERATIONS},
  }})
  harness.runtime.emit_response({{text: "STATUS:" + to_string(result.status)}})
}}
"#,
            );
            std::fs::write(&pipeline_path, source).expect("write served-cancel-loop pipeline");

            // Default mode: `start_acp_channel_session_with_config` does NOT
            // send `session/set_mode`, so this session runs under `ask`.
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id.clone(),
                        "prompt": [{"type": "text", "text": "wait"}],
                    },
                }))
                .expect("send session/prompt");

            // Wait until the loop is demonstrably live before cancelling, so a
            // turn that never started cannot pass as a turn that was stopped.
            let mut chunks_at_cancel = 0usize;
            let mut early_result = serde_json::Value::Null;
            for _ in 0..400 {
                while let Ok(line) = response_rx.try_recv() {
                    let message: serde_json::Value =
                        serde_json::from_str(&line).expect("ACP JSON line");
                    if answer_host_capabilities(&request_tx, &message) {
                        continue;
                    }
                    if is_agent_message_chunk(&message) {
                        chunks_at_cancel += 1;
                    } else if message["id"] == 3 {
                        early_result = message["result"].clone();
                    }
                }
                if chunks_at_cancel >= 2 || !early_result.is_null() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(
                early_result.is_null(),
                "prompt finished before the cancel could be sent: {early_result}"
            );
            assert!(
                chunks_at_cancel >= 2,
                "the served agent loop never streamed 2 assistant chunks, so there was \
                 no live turn to cancel; saw {chunks_at_cancel}"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {"sessionId": session_id.clone()},
                }))
                .expect("send session/cancel notification");

            let mut result = serde_json::Value::Null;
            for _ in 0..256 {
                let message = recv_json(&mut response_rx).await;
                if answer_host_capabilities(&request_tx, &message) {
                    continue;
                }
                if message["id"] == 3 {
                    result = message["result"].clone();
                    break;
                }
            }

            drop(request_tx);
            let _ = server.await;

            assert_eq!(
                result["stopReason"], "cancelled",
                "session/cancel must reach a live served turn; got {result}"
            );
            assert_eq!(
                result["_meta"]["harn"]["terminal"]["kind"], "user_cancelled",
                "a cancelled served turn must seal a user_cancelled terminal; got {result}"
            );
        })
        .await;
}
