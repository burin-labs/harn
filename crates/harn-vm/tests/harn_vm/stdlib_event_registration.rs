//! Canonical host-path regression for stdlib event registration.
//!
//! These event names used to be rejected by `__host_agent_emit_event`; their
//! stdlib callers swallowed the error, so neither subscribers nor the live
//! transcript could observe them.

use harn_vm::value::VmError;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    execute_compiled(source)
}

fn execute_compiled(source: &str) -> Result<String, String> {
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

pipeline main(harness: Harness, task: unknown) {
  const session = harness.agent.open("stdlib-event-registration")
  const captured = agent_capture_events(
    harness.agent,
    session,
    fn() {
      agent_emit_event(
        harness.agent,
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
        harness.agent,
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
        harness.agent,
        session,
        "pack_thinking_stripped",
        {model: "claude-opus-adaptive", requested: "high", reason: "claude_opus_adaptive"},
      )
      agent_emit_event(
        harness.agent,
        session,
        "self_consistency_tie",
        {
          answer: "alpha",
          total: 4,
          distribution: [{answer: "alpha", count: 2}, {answer: "beta", count: 2}],
        },
      )
      agent_emit_event(
        harness.agent,
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
  harness.stdio.log(len(captured.events))
  for event in captured.events {
    harness.stdio.log(event.type)
  }
  const transcript = harness.agent.snapshot(session)
  for event_type in [
    "require_successful_tools_violation",
    "final_wrapup",
    "pack_thinking_stripped",
    "self_consistency_tie",
    "code_librarian_query_nl_fallback",
  ] {
    harness.stdio.log(len(transcript_events_by_kind(transcript, event_type)))
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

fn run_with_host_ingest_warnings(
    source: &str,
) -> (Result<String, String>, Vec<harn_vm::events::LogEvent>) {
    use harn_vm::events::{add_event_sink, clear_event_sinks, reset_event_sinks, CollectorSink};
    use std::rc::Rc;
    harn_vm::reset_thread_local_state();
    let sink = Rc::new(CollectorSink::new());
    clear_event_sinks();
    add_event_sink(sink.clone());
    let result = execute_compiled(source);
    let warnings = sink
        .logs
        .borrow()
        .iter()
        .filter(|event| event.category == "host_event_ingest")
        .cloned()
        .collect();
    reset_event_sinks();
    (result, warnings)
}

#[test]
fn unknown_host_event_type_completes_the_run_and_warns_once_per_session() {
    let source = r#"
import { agent_capture_events } from "std/agent/events"
import { agent_emit_event } from "std/agent/state"

pipeline main(harness: Harness, task: unknown) {
  const session = harness.agent.open("unknown-event-warn")
  const captured = agent_capture_events(
    harness.agent,
    session,
    fn() {
      agent_emit_event(harness.agent, session, "zz_probe_never_seen", {ok: true})
      agent_emit_event(harness.agent, session, "zz_probe_never_seen", {again: true})
      agent_emit_event(harness.agent, session, "progress_reported", {})
      return nil
    },
  )
  harness.stdio.log("completed")
  harness.stdio.log(len(captured.events))
  for event in captured.events {
    harness.stdio.log(event.type)
  }
  const transcript = harness.agent.snapshot(session)
  harness.stdio.log(len(transcript_events_by_kind(transcript, "zz_probe_never_seen")))
  harness.stdio.log(len(transcript_events_by_kind(transcript, "progress_reported")))
}
"#;
    let (output, warnings) = run_with_host_ingest_warnings(source);
    let lines = output
        .expect("Harn source executes")
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["completed", "1", "progress_reported", "0", "1"],
        "unknown type must not kill the run, reach subscribers, or journal"
    );
    assert_eq!(
        warnings.len(),
        1,
        "exactly one warning per unknown type per session, got {warnings:?}"
    );
    assert_eq!(warnings[0].level, harn_vm::events::EventLevel::Warn);
    assert!(
        warnings[0].message.contains("`zz_probe_never_seen`"),
        "warning must name the type: {}",
        warnings[0].message
    );
}

#[test]
fn malformed_known_host_event_type_is_still_fatal() {
    let error = run(r#"
import { agent_emit_event } from "std/agent/state"

pipeline main(harness: Harness, task: unknown) {
  const session = harness.agent.open("malformed-known-event")
  agent_emit_event(
    harness.agent,
    session,
    "iteration_start",
    {iteration: "not-a-number"},
  )
  harness.stdio.log("should-not-reach")
}
"#)
    .expect_err("malformed payload of a known type must fail the run");
    assert!(
        error.contains("invalid `iteration_start` payload") || error.contains("iteration_start"),
        "fatal error must name the known type, got {error}"
    );
}
