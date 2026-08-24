//! Full-loop regression for terminal background-command replay.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use harn_vm::bridge::HostBridge;
use harn_vm::value::VmError;

fn run_with_bridge(source: &str) -> Result<String, String> {
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
                let bridge = Arc::new(HostBridge::from_parts(
                    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(())),
                    1,
                ));
                harn_vm::llm::install_current_host_bridge(bridge);
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|error: VmError| format!("{error:?}"));
                harn_vm::llm::clear_current_host_bridge();
                result?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .map(str::to_string)
        .collect()
}

#[test]
fn green_background_verify_then_done_closes_once_despite_terminal_replay() {
    let raw = run_with_bridge(
        r###"
import { agent_capture_events } from "std/agent/events"
import { agent_loop } from "std/agent/loop"

pipeline main(harness: Harness, task: unknown) {
  const session = "terminal-ledger-" + harness.random.uuid()
  const llm_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: session + "/llm_calls", initial: 0},
  )
  const dispatches = harness.runtime.shared_cell(
    {scope: "task_group", key: session + "/dispatches", initial: 0},
  )
  const replays = harness.runtime.shared_cell(
    {scope: "task_group", key: session + "/replays", initial: 0},
  )
  const terminal = json_stringify(
    {
      handle_id: "hto-green-verify",
      status: "completed",
      exit_code: 0,
      duration_ms: 6460,
      stdout: "13 passed in 6.46s",
      output_path: "/tmp/verify.log",
    },
  )
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "start_verify",
    "Start the post-write verifier in the background.",
    {
      parameters: {},
      handler: { _args -> return "unused: tool_caller owns the test dispatch" },
      annotations: {kind: "execute", side_effect_level: "read_only"},
    },
  )
  const tool_caller = { envelope, _next ->
    const snapshot = harness.runtime.shared_snapshot(dispatches)
    harness.runtime.shared_cas(dispatches, snapshot, snapshot.value + 1)
    return {
      ok: true,
      status: "running",
      tool_name: envelope.tool_name,
      tool_call_id: envelope.call_id,
      handle_id: "hto-green-verify",
      command_or_op_descriptor: "pytest -q",
      output_offset: 0,
      byte_count: 0,
      stderr_byte_count: 0,
      silence_ms: 0,
      stdout: "",
      result: {status: "running", handle_id: "hto-green-verify"},
    }
  }
  harness.agent.subscribe(
    session,
    { event ->
      if event.type == "typed_checkpoint"
        && event?.checkpoint?.kind == "command_hold"
        && event?.checkpoint?.outcome == "digest" {
        const snapshot = harness.runtime.shared_snapshot(replays)
        if snapshot.value == 0 {
          harness.runtime.shared_cas(replays, snapshot, 1)
          harness.agent.post_event(
            session,
            "tool_result",
            terminal,
            "test.terminal_replay",
          )
        }
      }
    },
  )
  const caller = { _call ->
    const snapshot = harness.runtime.shared_snapshot(llm_calls)
    const call_number = snapshot.value + 1
    harness.runtime.shared_cas(llm_calls, snapshot, call_number)
    if call_number == 1 {
      return {
        ok: true,
        value: {
          text: "",
          tool_calls: [{id: "verify-1", name: "start_verify", arguments: {}}],
          provider: "mock",
          model: "mock",
        },
      }
    }
    if call_number == 2 {
      // This terminal wakes the real command-ledger hold. The subscriber above
      // then replays the same immutable receipt after that hold consumed it.
      harness.agent.post_event(session, "tool_result", terminal, "test.terminal")
    }
    return {
      ok: true,
      value: {
        text: "Verification is green. ##DONE##",
        tool_calls: [],
        provider: "mock",
        model: "mock",
      },
    }
  }
  const captured = agent_capture_events(
    harness.agent,
    session,
    fn() {
      return agent_loop(
        harness,
        "Make the change and verify it.",
        nil,
        {
          provider: "mock",
          session_id: session,
          tools: tools,
          tool_format: "native",
          llm_caller: caller,
          tool_caller: tool_caller,
          loop_until_done: true,
          done_sentinel: "##DONE##",
          done_judge: false,
          final_wrapup: false,
          iteration_budget: {mode: "fixed", initial: 8, max: 8},
          stall_diagnostics: {
            enabled: true,
            repeat_success: 2,
            no_progress_messages: 2,
            hard_stop_after_trips: 2,
            inject_feedback: false,
          },
        },
      )
    },
  )
  const terminals = transcript_events_by_kind(captured.result.transcript, "agent_run_terminal")
  const thrash = captured.events.filter(
    { event -> event?.reason == "thrash_hard_stop" || event?.checkpoint?.receipt_kind == "thrash_hard_stop_recovery" },
  )
  const holds = captured.events.filter(
    { event -> event.type == "typed_checkpoint" && event?.checkpoint?.kind == "command_hold" },
  )
  harness.stdio.log(captured.result.status)
  harness.stdio.log(captured.result.stop_reason)
  harness.stdio.log(harness.runtime.shared_get(llm_calls))
  harness.stdio.log(harness.runtime.shared_get(dispatches))
  harness.stdio.log(harness.runtime.shared_get(replays))
  harness.stdio.log(len(terminals))
  harness.stdio.log(len(thrash))
  harness.stdio.log(len(holds))
}
"###,
    )
    .expect("full loop must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(lines[1], "sentinel", "lines: {lines:?}");
    assert_eq!(
        lines[2], "2",
        "terminal success and exact DONE close without another inference; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "1",
        "the verifier dispatches once; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "1",
        "the terminal receipt replays once; lines: {lines:?}"
    );
    assert_eq!(
        lines[5], "1",
        "terminal completion emits once; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "0",
        "terminal replay must not trigger thrash; lines: {lines:?}"
    );
    assert_eq!(
        lines[7], "1",
        "the command hold must fire exactly once; lines: {lines:?}"
    );
}
