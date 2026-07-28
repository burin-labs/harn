//! Canonical host-path regression for stdlib event registration.
//!
//! These event names used to be rejected by `__host_agent_emit_event`; their
//! stdlib callers swallowed the error, so neither subscribers nor the live
//! transcript could observe them.

use harn_vm::value::VmError;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|error: VmError| format!("{error:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn output_lines(source: &str) -> Vec<String> {
    run(source)
        .expect("Harn source executes")
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .map(str::to_string)
        .collect()
}

#[test]
fn documented_stdlib_events_reach_subscribers_and_live_transcript() {
    let lines = output_lines(
        r#"
import { agent_capture_events } from "std/agent/events"
import { agent_emit_event } from "std/agent/state"

pipeline main(task) {
  const session = agent_session_open("stdlib-event-registration")
  const captured = agent_capture_events(
    session,
    fn() {
      agent_emit_event(
        session,
        "require_successful_tools_violation",
        {
          kind: "tool_gap",
          source: "agent_loop.require_successful_tools",
          actor: nil,
          run_id: session,
          redacted_summary: "missing edit",
          recurrence_hints: ["missing_required_tools=1"],
          metadata: {
            missing_required_tools: ["edit"],
            successful_tool_names: [],
            iterations: 2,
          },
        },
      )
      agent_emit_event(
        session,
        "final_wrapup",
        {
          final_status: "max_iterations",
          stop_reason: "iteration_limit",
          iteration: 4,
          host_directive: false,
          terminal_kind: "max_iterations",
        },
      )
      agent_emit_event(
        session,
        "pack_thinking_stripped",
        {model: "claude-opus-adaptive", requested: "high", reason: "claude_opus_adaptive"},
      )
      agent_emit_event(
        session,
        "self_consistency_tie",
        {
          answer: "alpha",
          total: 4,
          distribution: [{answer: "alpha", count: 2}, {answer: "beta", count: 2}],
        },
      )
      agent_emit_event(
        session,
        "code_librarian_query_nl_fallback",
        {
          attempted_cypher: nil,
          mcts_depth: 3,
          mcts_expansions: 9,
          result_count: 2,
          text: "where is session recovery implemented?",
        },
      )
      return nil
    },
  )
  log(len(captured.events))
  for event in captured.events {
    log(event.type)
  }
  const transcript = agent_session_snapshot(session)
  for event_type in [
    "require_successful_tools_violation",
    "final_wrapup",
    "pack_thinking_stripped",
    "self_consistency_tie",
    "code_librarian_query_nl_fallback",
  ] {
    log(len(transcript_events_by_kind(transcript, event_type)))
  }
}
"#,
    );

    assert_eq!(
        lines,
        vec![
            "5",
            "require_successful_tools_violation",
            "final_wrapup",
            "pack_thinking_stripped",
            "self_consistency_tie",
            "code_librarian_query_nl_fallback",
            "1",
            "1",
            "1",
            "1",
            "1",
        ],
    );
}
