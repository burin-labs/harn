#![recursion_limit = "256"]
//! Coverage for the forced terminal wrap-up turn (agent-loop-wrapup).
//!
//! When the agent loop terminates on iteration/budget exhaustion *while the
//! model was still calling tools*, the surfaced final text would otherwise be
//! a dangling tool-call turn with no clean `<user_response>` + completion
//! sentinel. The loop now fires exactly one tool-less LLM call to elicit a real
//! final answer, records it, and surfaces it as the run's `text`.
//!
//! These tests stub the LLM via the `llm_caller` seam (mirroring
//! `agent_loop_steering_seams.rs`) and count calls / inspect surfaced text to
//! prove:
//!   (a) exhaustion mid-tool-use fires the wrap-up and the sentinel lands;
//!   (b) a natural `done` exit fires NO wrap-up;
//!   (c) `final_wrapup: false` disables it.

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
        .map_err(|e| e.to_string())?;
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
                harn_vm::llm::install_current_host_bridge(bridge.clone());
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"));
                harn_vm::llm::clear_current_host_bridge();
                result?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn agent_loop_emits_llm_call_start_checkpoint_for_main_call() {
    let raw = run_with_bridge(
        r#"
import { agent_capture_events } from "std/agent/events"

fn llm_call_start_events(events) {
  return events.filter({ event -> event.type == "typed_checkpoint" && event.checkpoint?.kind == "llm_call_start" })
}

pipeline main(task) {
  const session = "agent-loop-llm-call-start-" + uuid()
  const caller = { call -> return {
    ok: true,
    value: {
      text: "<user_response>done</user_response>\n<done>##DONE##</done>",
      provider: call?.opts?.provider ?? "",
      model: call?.opts?.model ?? "",
      input_tokens: 0,
      output_tokens: 0,
    },
  } }
  const captured = agent_capture_events(
    session,
    fn() { return agent_loop(
      "finish",
      nil,
      {
        provider: "mock",
        model: "checkpoint-model",
        session_id: session,
        llm_caller: caller,
        loop_until_done: true,
        done_judge: false,
        max_iterations: 1,
        tool_format: "text",
      },
    ) },
  )
  const starts = llm_call_start_events(captured.events)
  const checkpoint = starts[0].checkpoint
  log(captured.result.status)
  log(len(starts))
  log(checkpoint.kind)
  log(checkpoint.iteration)
  log(checkpoint.attempt)
  log(checkpoint.provider)
  log(checkpoint.model)
  log(checkpoint.tool_format)
  log(checkpoint.final_wrapup)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(lines[1], "1", "expected one checkpoint; lines: {lines:?}");
    assert_eq!(lines[2], "llm_call_start", "lines: {lines:?}");
    assert_eq!(lines[3], "1", "lines: {lines:?}");
    assert_eq!(lines[4], "1", "lines: {lines:?}");
    assert_eq!(lines[5], "mock", "lines: {lines:?}");
    assert_eq!(lines[6], "checkpoint-model", "lines: {lines:?}");
    assert_eq!(lines[7], "text", "lines: {lines:?}");
    assert_eq!(lines[8], "false", "lines: {lines:?}");
}

/// Pipeline whose stub LLM always emits a tool call until `max_iterations`
/// runs out. The stub distinguishes the wrap-up call via the `_final_wrapup`
/// flag the loop sets on `llm_opts`; on that call it returns a clean
/// `<user_response>` + `<done>` sentinel. Logs:
///   1. result.status
///   2. number of tool-calling (loop) LLM calls
///   3. number of wrap-up LLM calls (0 or 1)
///   4. whether result.text carries the wrap-up summary (the `<user_response>`
///      body; the `<done>` sentinel itself is stripped from surfaced text by
///      the response-protocol sanitizer, exactly as for a natural `done` turn)
fn exhaustion_pipeline(session_id: &str, final_wrapup_opt: &str) -> String {
    format!(
        r#"
pipeline main(task) {{
  clear_tool_hooks()
  let registry = tool_registry()
  let tools = tool_define(
    registry,
    "keep_exploring",
    "Test stand-in for a tool the model keeps calling.",
    {{parameters: {{}}, handler: {{ _args -> return "explored" }}}},
  )
  let tool_calls_counter = shared_cell(
    {{scope: "task_group", key: "wrapup-tool-calls-{session_id}", initial: 0}},
  )
  let wrapup_counter = shared_cell(
    {{scope: "task_group", key: "wrapup-final-calls-{session_id}", initial: 0}},
  )
  let mock_llm = {{ _call ->
    if _call?.opts?._final_wrapup == true {{
      let wsnap = shared_snapshot(wrapup_counter)
      shared_cas(wrapup_counter, wsnap, wsnap.value + 1)
      return {{
        ok: true,
        value: {{
          text: "<user_response>Refactored the parser and ran the tests.</user_response>\n<done>FINISHED</done>",
          tool_calls: [],
          provider: "mock",
          model: "mock",
        }},
      }}
    }}
    let snap = shared_snapshot(tool_calls_counter)
    shared_cas(tool_calls_counter, snap, snap.value + 1)
    return {{
      ok: true,
      value: {{
        text: "",
        tool_calls: [{{id: "call_explore", name: "keep_exploring", arguments: {{}}}}],
        provider: "mock",
        model: "mock",
      }},
    }}
  }}
  let result = agent_loop(
    "do the work",
    nil,
    {{
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      session_id: "{session_id}",
      llm_caller: mock_llm,
{final_wrapup_opt}
    }},
  )
  log(result.status)
  log(shared_get(tool_calls_counter))
  log(shared_get(wrapup_counter))
  log(contains(result.text, "Refactored the parser"))
}}
"#
    )
}

/// Pipeline whose stub LLM finishes cleanly on the first turn (no tool call,
/// emits the sentinel). The natural `done` exit must NOT trigger a wrap-up.
fn clean_done_pipeline(session_id: &str) -> String {
    format!(
        r#"
pipeline main(task) {{
  clear_tool_hooks()
  const wrapup_counter = shared_cell(
    {{scope: "task_group", key: "clean-done-wrapup-{session_id}", initial: 0}},
  )
  const mock_llm = {{ _call ->
    if _call?.opts?._final_wrapup == true {{
      const wsnap = shared_snapshot(wrapup_counter)
      shared_cas(wrapup_counter, wsnap, wsnap.value + 1)
    }}
    return {{
      ok: true,
      value: {{
        text: "<user_response>All done.</user_response>\n<done>FINISHED</done>",
        tool_calls: [],
        provider: "mock",
        model: "mock",
      }},
    }}
  }}
  const result = agent_loop(
    "do the work",
    nil,
    {{
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      session_id: "{session_id}",
      llm_caller: mock_llm,
    }},
  )
  log(result.status)
  log(shared_get(wrapup_counter))
  log(contains(result.text, "All done."))
}}
"#
    )
}

#[test]
fn exhaustion_mid_tool_use_fires_wrapup_and_surfaces_sentinel() {
    let raw =
        run_with_bridge(&exhaustion_pipeline("wrapup-exhaustion", "")).expect("script must run");
    let lines = out_lines(&raw);
    // The loop exhausted iterations while still calling tools.
    assert_eq!(
        lines[0], "budget_exhausted",
        "expected budget_exhausted; lines: {lines:?}"
    );
    // Three tool-calling loop turns (max_iterations = 3).
    assert_eq!(
        lines[1], "3",
        "expected three loop LLM calls; lines: {lines:?}"
    );
    // Exactly one wrap-up call.
    assert_eq!(
        lines[2], "1",
        "expected exactly one wrap-up LLM call; lines: {lines:?}"
    );
    // The surfaced final text is the wrap-up summary, not the dangling
    // tool-call turn.
    assert_eq!(
        lines[3], "true",
        "expected surfaced text to contain the wrap-up summary; lines: {lines:?}"
    );
}

#[test]
fn clean_done_exit_does_not_fire_wrapup() {
    let raw = run_with_bridge(&clean_done_pipeline("wrapup-clean-done")).expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "expected done; lines: {lines:?}");
    // No wrap-up call — the model already produced a clean terminal turn.
    assert_eq!(
        lines[1], "0",
        "expected zero wrap-up calls on a clean done; lines: {lines:?}"
    );
    // The naturally produced text is surfaced as-is.
    assert_eq!(
        lines[2], "true",
        "expected surfaced text to contain the natural final answer; lines: {lines:?}"
    );
}

#[test]
fn final_wrapup_false_disables_the_wrapup_turn() {
    let raw = run_with_bridge(&exhaustion_pipeline(
        "wrapup-disabled",
        "      final_wrapup: false,",
    ))
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(
        lines[0], "budget_exhausted",
        "expected budget_exhausted; lines: {lines:?}"
    );
    assert_eq!(
        lines[1], "3",
        "expected three loop LLM calls; lines: {lines:?}"
    );
    // Wrap-up is opted out — no extra call.
    assert_eq!(
        lines[2], "0",
        "expected zero wrap-up calls when final_wrapup:false; lines: {lines:?}"
    );
    // Without a wrap-up, the surfaced text is the dangling tool-call turn,
    // which carries no summary.
    assert_eq!(
        lines[3], "false",
        "expected no wrap-up summary in surfaced text when disabled; lines: {lines:?}"
    );
}
