//! A `json`-format tool call is hidden from the reply a host shows, and a call
//! to a tool the agent does not have still reaches the host as a failed tool
//! row. Together they mean hiding the fence never makes a call disappear.
//!
//! Driven over the real ACP wire with the provider mock armed outside the
//! governed Harn source, the same way `served_agent_turn` does.

use super::*;

struct MockModeGuard;

impl Drop for MockModeGuard {
    fn drop(&mut self) {
        harn_vm::llm::clear_cli_llm_mock_mode();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_json_tool_fence_is_hidden_and_an_undeclared_call_still_shows_as_failed() {
    served_tool_fence("nuke_repo", false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_declared_tool_call_publishes_only_the_accepted_answer() {
    served_tool_fence("inspect", true).await;
}

async fn served_tool_fence(tool: &str, accepted: bool) {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mocks = [
                format!("Checking first.\n\n```tool\n{{\"name\":\"{tool}\",\"args\":{{\"path\":\"a\"}}}}\n```"),
                if accepted { "Done. ##DONE##" } else { "Done." }.to_string(),
            ]
            .into_iter()
            .map(|text| {
                harn_vm::llm::parse_llm_mock_value(&serde_json::json!({
                    "text": text,
                    "model": "fence-proof",
                    "provider": "mock",
                }))
                .expect("mock fixture")
            })
            .collect();
            harn_vm::llm::install_cli_llm_mocks(mocks);
            let _mock_mode = MockModeGuard;

            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline = dir.path().join("fence.harn");
            std::fs::write(
                &pipeline,
                r#"import { agent_loop } from "std/agent/loop"
pipeline default(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "inspect", "Inspect a path.", {
    handler: { args -> "observed " + (args?.path ?? "") },
    parameters: {path: {type: "string"}},
    returns: {type: "string"},
    annotations: {kind: "read", side_effect_level: "read_only"},
  })
  const result = agent_loop(harness, prompt, nil, {
    provider: "mock",
    model: "fence-proof",
    tool_format: "json",
    loop_until_done: true,
    max_iterations: 3,
    max_nudges: 2,
    tools: tools,
  })
  const calls = harness.llm.mock_calls()
  assert(len(calls) >= 2, "both actor turns must reach the provider boundary")
  assert(contains(calls[0].system, "Agent completion contract"), "completion contract was injected")
  assert(contains(calls[0].system, "include `##DONE##` exactly once"), "JSON route explicitly requires the completion marker")
  return result
}
"#,
            )
            .expect("write pipeline");

            let (request_tx, mut response_rx, _server, session_id) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "Inspect a."}],
                    },
                }))
                .expect("send session/prompt");

            let mut updates = Vec::new();
            let response = harn_clock::test_support::within("session/prompt response", async {
                loop {
                    let line = response_rx.recv().await.expect("ACP channel closed");
                    let message: serde_json::Value = serde_json::from_str(&line).expect("JSON");
                    if message["method"] == "host/capabilities" {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("answer host capabilities");
                    } else if message["id"] == 2 {
                        return message;
                    } else if message["method"] == "session/update" {
                        updates.push(message["params"]["update"].clone());
                    }
                }
            })
            .await;
            assert!(response["error"].is_null(), "{response:#}");

            let shown: Vec<String> = updates
                .iter()
                .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
                .filter_map(|update| {
                    let content = &update["content"];
                    content["_meta"]["harn"]["visible_text"]
                        .as_str()
                        .or_else(|| content["text"].as_str())
                        .map(str::to_string)
                })
                .collect();
            assert!(
                shown.iter().all(|text| !text.contains("Checking first.")),
                "pre-tool narration must remain a private draft: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .all(|text| !text.contains("```tool") && !text.contains(tool)),
                "the call must not be shown again as raw JSON: {shown:?}"
            );
            if accepted {
                assert_eq!(shown, ["Done."], "publish the accepted answer exactly once");
                let completed = updates
                    .iter()
                    .position(|update| {
                        update["sessionUpdate"] == "tool_call_update"
                            && update["title"] == "inspect"
                            && update["status"] == "completed"
                            && update["rawOutput"] == "observed a"
                    })
                    .expect("declared inspection executed and returned its actual result");
                let published = updates
                    .iter()
                    .position(|update| update["sessionUpdate"] == "agent_message_chunk")
                    .expect("accepted answer callback");
                assert!(completed < published, "the actual tool result precedes publication");
            } else {
                assert!(shown.is_empty(), "a failed task has no accepted answer: {shown:?}");
            }

            let rows: Vec<&serde_json::Value> = updates
                .iter()
                .filter(|update| {
                    update["sessionUpdate"] == "tool_call"
                        || update["sessionUpdate"] == "tool_call_update"
                })
                .collect();
            assert!(
                rows.iter()
                    .any(|row| row.to_string().contains(tool) && row["status"] == if accepted { "completed" } else { "failed" }),
                "the call outcome must reach the host independently of draft text: {rows:#?}"
            );
            drop(request_tx);
        })
        .await;
}
