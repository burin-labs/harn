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

/// Tool-calling exhaustion pipeline that ALSO passes a host `wrapup_directive` +
/// `wrapup_context` and captures, on the wrap-up call, the `system` prompt and
/// the LAST message the loop appended. Proves the host-directive path renders the
/// directive as a trailing user message while leaving the system prompt free of
/// the directive (byte-identical prefix for cache reuse), and that the structured
/// context + outcome-authority constraint reach the model. Logs:
///   1. result.status
///   2. wrap-up call count
///   3. system contains the host directive marker (expect false)
///   4. last message contains the host directive marker (expect true)
///   5. last message contains the outcome-authority phrase (expect true)
///   6. last message contains the context key `terminal_kind` (expect true)
///   7. last message contains the context label (expect true)
///   8. surfaced text carries the wrap-up summary (expect true)
fn host_directive_capture_pipeline(session_id: &str) -> String {
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
  let wrapup_counter = shared_cell({{scope: "task_group", key: "hd-wc-{session_id}", initial: 0}})
  let sys_cell = shared_cell({{scope: "task_group", key: "hd-sys-{session_id}", initial: ""}})
  let lastmsg_cell = shared_cell({{scope: "task_group", key: "hd-last-{session_id}", initial: ""}})
  let mock_llm = {{ _call ->
    if _call?.opts?._final_wrapup == true {{
      let wsnap = shared_snapshot(wrapup_counter)
      shared_cas(wrapup_counter, wsnap, wsnap.value + 1)
      let msgs = _call?.opts?.messages ?? []
      let last = if len(msgs) > 0 {{ msgs[len(msgs) - 1] }} else {{ {{}} }}
      let ssnap = shared_snapshot(sys_cell)
      shared_cas(sys_cell, ssnap, to_string(_call?.system ?? ""))
      let lsnap = shared_snapshot(lastmsg_cell)
      shared_cas(lastmsg_cell, lsnap, to_string(last?.content ?? ""))
      return {{
        ok: true,
        value: {{
          text: "<user_response>Stopped at the budget; parser half-migrated, tests red.</user_response>\n<done>FINISHED</done>",
          tool_calls: [],
          provider: "mock",
          model: "mock",
        }},
      }}
    }}
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
      max_iterations: 2,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      session_id: "{session_id}",
      llm_caller: mock_llm,
      wrapup_directive: "PRODUCT_SADPATH_DIRECTIVE_XYZ",
      wrapup_context: {{terminal_kind: "budget_exhausted", files_touched: [{{path: "parser.rs", added_lines: 12}}]}},
    }},
  )
  log(result.status)
  log(shared_get(wrapup_counter))
  log(contains(shared_get(sys_cell), "PRODUCT_SADPATH_DIRECTIVE_XYZ"))
  log(contains(shared_get(lastmsg_cell), "PRODUCT_SADPATH_DIRECTIVE_XYZ"))
  log(contains(shared_get(lastmsg_cell), "narrate it truthfully"))
  log(contains(shared_get(lastmsg_cell), "terminal_kind"))
  log(contains(shared_get(lastmsg_cell), "Terminal context (authoritative facts)"))
  log(contains(result.text, "parser half-migrated"))
}}
"#
    )
}

/// No-tool exhaustion pipeline: the model emits non-done prose and never calls a
/// tool, so the loop exhausts at `last_tool_count == 0`. Parameterized on the
/// wrap-up opts so the same terminal can be measured with and without a host
/// directive. Logs status and the wrap-up call count.
fn no_tool_terminal_pipeline(session_id: &str, wrapup_opts: &str) -> String {
    format!(
        r#"
pipeline main(task) {{
  clear_tool_hooks()
  let wrapup_counter = shared_cell({{scope: "task_group", key: "nt-wc-{session_id}", initial: 0}})
  let mock_llm = {{ _call ->
    if _call?.opts?._final_wrapup == true {{
      let wsnap = shared_snapshot(wrapup_counter)
      shared_cas(wrapup_counter, wsnap, wsnap.value + 1)
      return {{
        ok: true,
        value: {{
          text: "<user_response>Ran out of budget before writing anything.</user_response>\n<done>FINISHED</done>",
          tool_calls: [],
          provider: "mock",
          model: "mock",
        }},
      }}
    }}
    return {{
      ok: true,
      value: {{text: "Still thinking, no action yet.", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}
  let result = agent_loop(
    "do the work",
    nil,
    {{
      provider: "mock",
      max_iterations: 2,
      loop_until_done: true,
      done_judge: false,
      done_sentinel: "FINISHED",
      session_id: "{session_id}",
      llm_caller: mock_llm,
{wrapup_opts}
    }},
  )
  log(result.status)
  log(shared_get(wrapup_counter))
}}
"#
    )
}

/// Host-directive exhaustion pipeline whose WRAP-UP call fails (`ok: false`).
/// Proves the host path degrades exactly like the default path: no summary is
/// recorded and the terminal outcome is unchanged. Logs status, wrap-up call
/// count, and whether the surfaced text carries a summary.
fn host_directive_degrade_pipeline(session_id: &str) -> String {
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
  let wrapup_counter = shared_cell({{scope: "task_group", key: "deg-wc-{session_id}", initial: 0}})
  let mock_llm = {{ _call ->
    if _call?.opts?._final_wrapup == true {{
      let wsnap = shared_snapshot(wrapup_counter)
      shared_cas(wrapup_counter, wsnap, wsnap.value + 1)
      return {{ok: false, status: "provider_error", error: {{message: "wrap-up provider down"}}}}
    }}
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
      max_iterations: 2,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      session_id: "{session_id}",
      llm_caller: mock_llm,
      wrapup_directive: "PRODUCT_SADPATH_DIRECTIVE_XYZ",
    }},
  )
  log(result.status)
  log(shared_get(wrapup_counter))
  log(contains(result.text, "PRODUCT_SADPATH_DIRECTIVE_XYZ"))
}}
"#
    )
}

#[test]
fn host_directive_rides_trailing_message_and_keeps_system_stable() {
    let raw = run_with_bridge(&host_directive_capture_pipeline("wrapup-host-directive"))
        .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "expected exactly one wrap-up call; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "false",
        "host directive must NOT be injected into the system prompt (prefix stays stable); lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "true",
        "host directive must ride the trailing user message; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "true",
        "the outcome-authority constraint must be appended to the host directive; lines: {lines:?}"
    );
    assert_eq!(
        lines[5], "true",
        "structured wrapup_context must be rendered into the trailing message; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "true",
        "the context block must carry its authoritative-facts label; lines: {lines:?}"
    );
    assert_eq!(
        lines[7], "true",
        "the host wrap-up summary must be surfaced as the run text; lines: {lines:?}"
    );
}

#[test]
fn tools_zero_terminal_fires_wrapup_only_with_host_directive() {
    // No host directive: a zero-tool terminal is NOT wrapped up (nothing the
    // generic path summarizes), byte-identical to today.
    let raw_off = run_with_bridge(&no_tool_terminal_pipeline("wrapup-notool-off", ""))
        .expect("script must run");
    let off = out_lines(&raw_off);
    assert_eq!(off[0], "budget_exhausted", "lines: {off:?}");
    assert_eq!(
        off[1], "0",
        "a zero-tool terminal must not fire wrap-up without a host directive; lines: {off:?}"
    );
    // Host directive present: the same zero-tool terminal DOES fire wrap-up,
    // because the host opted into sad-path coverage.
    let raw_on = run_with_bridge(&no_tool_terminal_pipeline(
        "wrapup-notool-on",
        "      wrapup_directive: \"PRODUCT_SADPATH_DIRECTIVE_XYZ\",",
    ))
    .expect("script must run");
    let on = out_lines(&raw_on);
    assert_eq!(on[0], "budget_exhausted", "lines: {on:?}");
    assert_eq!(
        on[1], "1",
        "a zero-tool terminal must fire wrap-up when a host directive is supplied; lines: {on:?}"
    );
}

#[test]
fn host_directive_wrapup_failure_degrades_silently() {
    let raw = run_with_bridge(&host_directive_degrade_pipeline("wrapup-host-degrade"))
        .expect("script must run");
    let lines = out_lines(&raw);
    // The terminal outcome is unchanged by a failed wrap-up call.
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    // The wrap-up was attempted exactly once (one call), then abandoned.
    assert_eq!(
        lines[1], "1",
        "expected one attempted wrap-up call; lines: {lines:?}"
    );
    // No directive text leaks into the surfaced run text on failure.
    assert_eq!(
        lines[2], "false",
        "a failed wrap-up must not surface the directive or any summary; lines: {lines:?}"
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
