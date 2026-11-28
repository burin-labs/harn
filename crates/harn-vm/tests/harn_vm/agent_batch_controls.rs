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
        completion_evidence_role: "observation",
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

#[test]
fn accepted_bridge_steering_retires_mixed_batch_proposals_without_executing_them() {
    let source = r#"
import { agent_capture_events } from "std/agent/events"
import { agent_loop } from "std/agent/loop"
import { agent_session_pending_injections, agent_session_push_user_message } from "std/agent/state"
import { with_llm_script } from "std/testing"

pipeline main(harness: Harness) {
  for mode in ["steer", "interrupt_immediate", "finish_step"] {
    const sid = "retained-bridge-" + harness.random.uuid()
    const edits = harness.runtime.atomic(0)
    const reads = harness.runtime.atomic(0)
    let tools = tool_registry()
    tools = tool_define(tools, "edit", "Change the workspace.", {
      parameters: {}, annotations: {kind: "edit", side_effect_level: "workspace_write"},
      handler: { _args -> harness.runtime.atomic_add(edits, 1) },
    })
    tools = tool_define(tools, "read", "Read workspace evidence.", {
      parameters: {}, annotations: {kind: "read", side_effect_level: "read_only"},
      handler: { _args ->
        harness.runtime.atomic_add(reads, 1)
        const id = agent_session_push_user_message(harness.agent, sid, {
          content: "Change direction before editing.", mode: mode,
        })
        require id != "", "the actual bridge must accept the control"
        require agent_session_pending_injections(harness.agent, sid).pendingCount == 1,
          "the accepted control must really be queued"
        return "Read complete."
      },
    })
    const captured = agent_capture_events(harness.agent, sid, { ->
      return with_llm_script(harness.llm, [{tool_calls: [
        {id: "edit", name: "edit", arguments: {}},
        {id: "read", name: "read", arguments: {}},
      ]}], { -> agent_loop(harness, "Read then edit.", nil, {
        provider: "mock", tools: tools, tool_format: "native", session_id: sid,
        loop_until_done: true, max_iterations: 1, final_wrapup: false,
      }) })
    })
    require harness.runtime.atomic_get(reads) == 1, "the read must be reached"
    require harness.runtime.atomic_get(edits) == 0, "accepted steering must prevent stale edit execution"
    require captured.events.any({ event ->
      event?.type == "tool_batch_proposal_resolution"
        && event.resolution.resolution == "invalidated"
        && event.resolution.reason == "control_changed"
        && event.resolution.pending_count == 0
    }), "the typed owner must retire the proposal at the accepted control boundary"
    const messages = captured.result.transcript.messages
    let paired = false
    for (index, message) in iter(messages).enumerate() {
      if message?.role == "assistant" && len(message?.tool_calls ?? []) == 2 {
        require messages[index + 1]?.role == "tool_result", "first native result must be paired"
        require messages[index + 2]?.role == "tool_result", "second native result must precede steering"
        paired = true
      }
    }
    require paired, "the actual native result sequence must be observed"
  }
  harness.stdio.println("retained_bridge_controls=ok")
}
"#;
    let output =
        crate::support::run_with_host_bridge(source).expect("retained proposal bridge fixture");
    assert!(output.contains("retained_bridge_controls=ok"), "{output}");
}
