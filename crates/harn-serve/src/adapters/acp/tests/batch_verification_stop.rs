//! A real session stop must interrupt an edit before its dependent verifier.

use super::*;

async fn run_batch(stop: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("operations");
    let marker_literal = format!(
        "\"{}\"",
        harn_lexer::escape_string_literal(&marker.to_string_lossy())
    );
    let pipeline = dir.path().join("batch.harn");
    std::fs::write(&pipeline, "pipeline default(harness: Harness) {}\n").expect("create pipeline");
    let (request_tx, mut response_rx, server, session_id) = start_acp_code_session_with_config(
        AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string()),
        serde_json::json!(dir.path()),
    )
    .await;
    let source = format!(
        r#"import {{ agent_loop }} from "std/agent/loop"
import {{ agent_emit_event, agent_session_await_inbox }} from "std/agent/state"
import {{ agent_tool_handler_result }} from "std/agent/tool_lifecycle"

pipeline default(harness: Harness, task: unknown) {{
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({{text: "", tool_calls: [
    {{id: "edit", name: "edit", arguments: {{}}}},
    {{id: "verify", name: "verify", arguments: {{}}}},
  ]}})
  let tools = tool_registry()
  tools = tool_define(tools, "edit", "Edit", {{parameters: {{}},
    annotations: {{kind: "edit", side_effect_level: "workspace_write"}},
    handler: {{ _args ->
      harness.fs.append({marker_literal}, "edit\n")
      agent_emit_event(harness.agent, "{session_id}", "progress_reported", {{message: "edit entered"}})
      if {stop} {{
        agent_session_await_inbox(harness.agent, "{session_id}", 60000)
      }}
      return agent_tool_handler_result("edited", {{mutation_status: "applied", requires_repair_before_verify: false}})
    }},
  }})
  tools = tool_define(tools, "verify", "Verify", {{parameters: {{}},
    annotations: {{kind: "execute", side_effect_level: "process_exec", completion_evidence_role: "verification"}},
    handler: {{ _args ->
      harness.fs.append({marker_literal}, "verify\n")
      return agent_tool_handler_result("verified", {{}})
    }},
  }})
  agent_loop(harness, "Edit then verify", nil, {{provider: "mock", tools: tools,
    session_id: "{session_id}", tool_format: "native", loop_until_done: true, max_iterations: 1}})
}}
"#
    );
    std::fs::write(&pipeline, source).expect("write pipeline");
    request_tx
        .send(serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
            "params":{"sessionId":session_id.clone(), "prompt":[{"type":"text", "text":"Edit then verify"}]}}))
        .expect("send prompt");

    let (result, entered, cancelled) = harn_clock::test_support::within("batch prompt", async {
        let mut entered = false;
        let mut cancelled = false;
        loop {
            let message = recv_json(&mut response_rx).await;
            if message["method"] == "host/capabilities" {
                reply_empty_result(&request_tx, &message);
            } else if message["method"] == "session/update"
                && message["params"]["update"]["_meta"]["harn"]["message"] == "edit entered"
            {
                entered = true;
                if stop {
                    request_tx
                        .send(
                            serde_json::json!({"jsonrpc":"2.0", "method":"session/cancel",
                        "params":{"sessionId":session_id.clone()}}),
                        )
                        .expect("send stop");
                    cancelled = true;
                }
            } else if message["id"] == 3 {
                break (message["result"].clone(), entered, cancelled);
            }
        }
    })
    .await;
    drop(request_tx);
    let _ = server.await;
    let operations = std::fs::read_to_string(&marker).expect("edit must run");
    assert_eq!(
        operations.lines().filter(|line| *line == "edit").count(),
        1,
        "{operations}"
    );
    assert_eq!(
        operations.lines().filter(|line| *line == "verify").count(),
        usize::from(!stop),
        "{operations}"
    );
    assert!(!result.is_null(), "prompt must finish");
    assert!(entered, "the edit must report that its handler started");
    if stop {
        assert!(cancelled, "stop must be sent after the edit starts");
        assert_eq!(result["stopReason"], "cancelled", "{result}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stop_during_edit_prevents_dependent_verifier() {
    harn_clock::test_support::with_local_tasks(async {
        run_batch(false).await;
        run_batch(true).await;
    })
    .await;
}
