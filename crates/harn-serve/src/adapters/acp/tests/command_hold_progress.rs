//! A held command narrates progress without replacing the user's plan.

use super::*;

#[cfg(unix)]
async fn command_hold_projection(stop: bool, cancel_command: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("command-gate");
    let fifo_literal = serde_json::to_string(&fifo).unwrap();
    let handle_path = dir.path().join("command-handle.txt");
    let handle_literal = serde_json::to_string(&handle_path).unwrap();
    let terminal_path = dir.path().join("collected-terminal.json");
    let terminal_literal = serde_json::to_string(&terminal_path).unwrap();
    let pipeline = dir.path().join("hold.harn");
    std::fs::write(&pipeline, "pipeline default(harness: Harness) {}\n").unwrap();
    let (request_tx, mut response_rx, server, session_id) = start_acp_code_session_with_config(
        AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string()),
        serde_json::json!(dir.path()),
    )
    .await;
    let source = format!(
        r###"import {{ agent_loop }} from "std/agent/loop"
import {{ agent_emit_event }} from "std/agent/state"
import {{ agent_tool_handler_result }} from "std/agent/tool_lifecycle"

pipeline default(harness: Harness) {{
  assert_eq(harness.tools.run_command({{argv: ["mkfifo", {fifo_literal}]}}).exit_code, 0)
  agent_emit_event(harness.agent, "{session_id}", "progress_reported", {{
    entries: [{{content: "Preserve the user's plan", status: "in_progress"}}], replace: true,
  }})
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({{text: "", tool_calls: [{{id: "start-command", name: "start", arguments: {{}}}}]}})
  harness.llm.mock_enqueue({{text: "##DONE##"}})
  let tools = tool_define(tool_registry(), "start", "Start command", {{parameters: {{}},
    handler: {{ _args ->
      const running = harness.tools.run_command({{argv: ["cat", {fifo_literal}], background_after_ms: 1}})
      assert_eq(running.status, "running")
      harness.fs.write_text({handle_literal}, running.handle_id)
      return agent_tool_handler_result("started", {{opaque_native_result: running}})
    }},
  }})
  tools = tool_define(tools, "collect", "Collect command", {{
    defer_loading: true,
    parameters: {{handle_id: {{type: "string"}}, timeout_ms: {{type: "integer"}}}},
    handler: {{ args ->
      const terminal = harness.tools.wait_command(args)
      harness.fs.write_text({terminal_literal}, json_stringify(terminal))
      if {cancel_command} {{
        assert_eq(terminal.status, "killed")
      }} else {{
        assert_eq(terminal.exit_code, 0)
        assert(terminal.stdout.contains("command-proof"))
      }}
      return agent_tool_handler_result("command-proof", {{run_outcome: terminal}})
    }},
  }})
  agent_loop(harness, "Run the command", nil, {{provider: "mock", tools: tools,
    session_id: "{session_id}", tool_format: "native", loop_until_done: true,
    done_sentinel: "##DONE##", max_iterations: 2, final_wrapup: false, turn_end_condition: false,
    command_wait: {{completion_tool: "collect"}}}})
}}
"###
    );
    std::fs::write(&pipeline, source).unwrap();
    request_tx
        .send(serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
            "params":{"sessionId":session_id.clone(), "prompt":[{"type":"text", "text":"Run the command"}]}}))
        .unwrap();
    let (result, plans, narrations) = harn_clock::test_support::within("command hold projection", async {
        let mut plans = Vec::new();
        let mut narrations = Vec::new();
        let mut released = false;
        loop {
            let message = recv_json(&mut response_rx).await;
            if message["method"] == "host/capabilities" {
                reply_empty_result(&request_tx, &message);
            } else if message["method"] == "session/update" {
                let update = &message["params"]["update"];
                if update["sessionUpdate"] == "plan" {
                    plans.push(update.clone());
                }
                if update["sessionUpdate"] == "progress" {
                    let text = update["_meta"]["harn"]["message"].as_str().unwrap_or("");
                    if text == "Waiting for running commands" && !released {
                        released = true;
                        if stop {
                            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "method":"session/cancel",
                                "params":{"sessionId":session_id.clone()}})).unwrap();
                        } else if cancel_command {
                            let handle = std::fs::read_to_string(&handle_path).expect("real native handle");
                            assert!(harn_hostlib::tools::long_running::cancel_handle(&handle));
                        } else {
                            std::fs::write(&fifo, "command-proof\n").expect("release the real command");
                        }
                    }
                    narrations.push(text.to_string());
                }
            } else if message["id"] == 3 {
                break (message["result"].clone(), plans, narrations);
            }
        }
    }).await;
    drop(request_tx);
    let _ = server.await;
    assert!(
        !result.is_null(),
        "a terminal prompt response must be measured"
    );
    assert_eq!(
        plans.len(),
        1,
        "wait progress must never replace the plan: {plans:?}"
    );
    assert_eq!(
        plans[0]["entries"][0]["content"],
        "Preserve the user's plan"
    );
    assert!(
        narrations
            .iter()
            .any(|text| text == "Waiting for running commands"),
        "actual hold narration: {narrations:?}"
    );
    if stop {
        assert_eq!(result["stopReason"], "cancelled", "{result}");
    } else {
        assert!(
            narrations.iter().any(|text| text == "Command finished"),
            "terminal narration: {narrations:?}"
        );
        assert_ne!(result["stopReason"], "cancelled", "{result}");
        let terminal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&terminal_path).expect("registered completion handler actually fired"),
        )
        .unwrap();
        assert_eq!(
            terminal["status"],
            if cancel_command {
                "killed"
            } else {
                "completed"
            }
        );
        if cancel_command {
            assert_eq!(terminal["exit_code"], -1);
            assert!(!terminal["output_path"].as_str().unwrap_or("").is_empty());
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn command_hold_preserves_the_plan_and_accepts_native_stop() {
    harn_clock::test_support::with_local_tasks(async {
        command_hold_projection(false, false).await;
        command_hold_projection(true, false).await;
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_native_command_wakes_its_active_hold_and_collects_the_terminal() {
    harn_clock::test_support::with_local_tasks(async {
        command_hold_projection(false, true).await;
    })
    .await;
}
