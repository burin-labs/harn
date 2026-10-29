//! The real host bridge must hold queued steering until both tool results exist.

#[test]
fn queued_bridge_steering_stops_verification_without_orphaning_tool_results() {
    let source = r#"
import { agent_capture_events } from "std/agent/events"
import { agent_loop } from "std/agent/loop"
import { agent_session_pending_injections, agent_session_push_user_message } from "std/agent/state"
import { agent_tool_handler_result } from "std/agent/tool_lifecycle"
import { with_llm_script } from "std/testing"

fn run_case(harness: Harness, mode: string) {
  const session = "batch-bridge-" + harness.random.uuid()
  const edits = harness.runtime.atomic(0)
  const checks = harness.runtime.atomic(0)
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "edit",
    "Edit the workspace.",
    {
      handler: { _args ->
        harness.runtime.atomic_add(edits, 1)
        if mode != "clear" {
          const id = agent_session_push_user_message(
            harness.agent,
            session,
            {content: "Change the task before checking.", mode: mode},
          )
          require id != "", "the bridge must accept the queued control"
          const pending = agent_session_pending_injections(harness.agent, session)
          require pending.pendingCount == 1, "the real bridge queue must contain the control"
        }
        return agent_tool_handler_result(
          "edited",
          {mutation_status: "applied", requires_repair_before_verify: false},
        )
      },
      parameters: {},
      annotations: {kind: "edit", side_effect_level: "workspace_write"},
    },
  )
  tools = tool_define(
    tools,
    "verify",
    "Verify the workspace.",
    {
      handler: { _args ->
        harness.runtime.atomic_add(checks, 1)
        return agent_tool_handler_result("verified", {})
      },
      parameters: {},
      annotations: {
        kind: "execute",
        side_effect_level: "process_exec",
        completion_evidence_role: "verification",
      },
    },
  )
  const captured = agent_capture_events(
    harness.agent,
    session,
    { ->
      return with_llm_script(
        harness.llm,
        [
          {
            tool_calls: [
              {id: "edit", name: "edit", arguments: {}},
              {id: "verify", name: "verify", arguments: {}},
            ],
          },
        ],
        { ->
          return agent_loop(
            harness,
            "Edit and verify.",
            nil,
            {
              provider: "mock",
              tools: tools,
              tool_format: "native",
              loop_until_done: true,
              max_iterations: 1,
              session_id: session,
            },
          )
        },
      )
    },
  )
  require harness.runtime.atomic_get(edits) == 1, "the edit must run"
  require harness.runtime.atomic_get(checks) == (mode == "clear" ? 1 : 0),
    "verification must run only when no bridge control is pending"
  const messages = captured.result?.transcript?.messages ?? []
  let paired = false
  for (index, message) in iter(messages).enumerate() {
    if message?.role == "assistant" && len(message?.tool_calls ?? []) == 2 {
      require messages[index + 1]?.role == "tool_result", "first native result must follow the call"
      require messages[index + 2]?.role == "tool_result",
        "second native result must precede steering"
      paired = true
      break
    }
  }
  require paired, "the two native tool results must be observed"
}

pipeline main(harness: Harness) {
  for mode in ["clear", "interrupt_immediate", "finish_step"] {
    run_case(harness, mode)
  }
  harness.stdio.println("bridge_batch_controls=ok")
}
"#;
    let output = crate::support::run_with_host_bridge(source).expect("agent loop fixture");
    assert!(output.contains("bridge_batch_controls=ok"), "{output}");
}
