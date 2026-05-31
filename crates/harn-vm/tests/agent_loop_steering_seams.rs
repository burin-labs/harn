#![recursion_limit = "256"]
//! Integration coverage for the `__agent_loop_checkpoint` seam catalog
//! (harn#2211) and the audit-only mode rename (harn#2212).
//!
//! Race-window coverage (#2211) — a host injects "STOP, do not push"
//! via `session/inject_reminder` after the model emits the `git push
//! --force` tool call but before dispatch; the reminder is invisible
//! until after the push completes. The "stopped" variant verifies the
//! pre-tool-dispatch checkpoint skips the dispatch and surfaces the
//! seam as a `loop_checkpoint` event with `dispatch_skipped: true`. The
//! "no-stop" variant verifies the same script *does* dispatch when no
//! injection is queued, so the test isn't just measuring absence.
//!
//! Audit-only mode (#2212) — `mode: "audit_only"` (formerly
//! `wait_for_completion`) drains at `loop_exit` only. The reminder
//! lands in the transcript audit, but the model never sees it: no
//! further LLM call runs. The "audit_only" variants verify the
//! transcript records the reminder, the loop's LLM call count is
//! unchanged versus a control run, and the `loop_exit` checkpoint
//! reports the delivery.

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

/// Harn pipeline that exercises `pre_tool_dispatch`. When
/// `push_stop_before_dispatch` is true the stub LLM caller queues an
/// `interrupt_immediate` bridge injection before returning the tool
/// call, simulating a host that pressed "STOP" mid-turn. Otherwise the
/// same script runs without the inject, so the tool dispatches
/// normally — that's the control case.
fn race_window_pipeline(session_id: &str, push_stop_before_dispatch: bool) -> String {
    let push_line = if push_stop_before_dispatch {
        format!(
            r#"      agent_session_push_bridge_injection(
        "{session_id}",
        {{body: "STOP — abort the push", mode: "interrupt_immediate", role_hint: "system"}},
      )"#
        )
    } else {
        String::new()
    };
    format!(
        r#"
import {{ agent_session_push_bridge_injection }} from "std/agent/state"

pipeline main(task) {{
  clear_tool_hooks()
  let registry = tool_registry()
  let tools = tool_define(
    registry,
    "would_force_push",
    "Test stand-in for an irreversible side-effect tool.",
    {{parameters: {{}}, handler: {{ _args -> return "would have force-pushed" }}}},
  )
  let iteration_state = shared_cell({{scope: "task_group", key: "iter-{session_id}", initial: 0}})
  let mock_llm = {{ _call ->
    let snap = shared_snapshot(iteration_state)
    let n = snap.value
    shared_cas(iteration_state, snap, n + 1)
    if n == 0 {{
{push_line}
      return {{
        ok: true,
        value: {{
          text: "",
          tool_calls: [{{id: "call_1", name: "would_force_push", arguments: {{}}}}],
          provider: "mock",
          model: "mock",
        }},
      }}
    }}
    return {{
      ok: true,
      value: {{text: "acknowledged ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}
  let result = agent_loop(
    "do the push",
    nil,
    {{
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "{session_id}",
      llm_caller: mock_llm,
    }},
  )
  log(result.status)
  let checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  var skipped_count = 0
  for event in checkpoints {{
    if event?.metadata?.dispatch_skipped == true {{
      skipped_count = skipped_count + 1
    }}
  }}
  log(skipped_count)
  let stats = transcript_stats(result.transcript)
  log(stats.tool_result_message_count)
}}
"#
    )
}

#[test]
fn pre_tool_dispatch_skips_when_interrupt_immediate_queued_mid_turn() {
    let raw = run_with_bridge(&race_window_pipeline("race-window-stops-dispatch", true))
        .expect("script must run");
    let lines = out_lines(&raw);
    // Status: done (model returned `##DONE##` on the second iteration).
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    // Exactly one loop_checkpoint event reported dispatch_skipped=true.
    assert_eq!(
        lines[1], "1",
        "expected one skipped dispatch; lines: {lines:?}"
    );
    // Zero tool_result events — the tool never ran.
    assert_eq!(lines[2], "0", "expected no tool dispatch; lines: {lines:?}");
}

#[test]
fn pre_tool_dispatch_dispatches_when_no_injection_queued() {
    let raw = run_with_bridge(&race_window_pipeline("race-window-no-stop", false))
        .expect("script must run");
    let lines = out_lines(&raw);
    // Status: done.
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    // Zero skipped dispatches — control case.
    assert_eq!(
        lines[1], "0",
        "expected no skipped dispatch; lines: {lines:?}"
    );
    // One tool_result event — the tool dispatched as expected.
    assert_eq!(
        lines[2], "1",
        "expected one tool dispatch; lines: {lines:?}"
    );
}

/// Harn pipeline that exercises the `audit_only` mode (formerly
/// `wait_for_completion`, harn#2212). The stub `llm_caller` returns
/// `##DONE##` on the first iteration; if `queue_audit_only` is true,
/// a system reminder is pushed via the bridge with `mode: "audit_only"`
/// before the loop runs. The script logs:
///
///   1. final status
///   2. the number of LLM calls observed by the caller stub
///   3. whether the reminder body appears in the post-exit transcript
///   4. the count of `loop_checkpoint` events whose `kind` is
///      `loop_exit` and whose `delivered` value is `>= 1`
///
/// The control run logs the same fields with `queue_audit_only = false`.
fn audit_only_pipeline(session_id: &str, queue_audit_only: bool) -> String {
    let push_block = if queue_audit_only {
        format!(
            r#"  agent_session_push_bridge_injection(
    "{session_id}",
    {{body: "audit trail: agent finished the merge", mode: "audit_only", role_hint: "system"}},
  )"#
        )
    } else {
        String::new()
    };
    format!(
        r#"
import {{ agent_session_push_bridge_injection }} from "std/agent/state"

pipeline main(task) {{
  clear_tool_hooks()
{push_block}
  let call_counter = shared_cell(
    {{scope: "task_group", key: "audit-only-llm-calls-{session_id}", initial: 0}},
  )
  let stub_llm = {{ _call ->
    let snap = shared_snapshot(call_counter)
    shared_cas(call_counter, snap, snap.value + 1)
    return {{
      ok: true,
      value: {{text: "all set ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}
  let result = agent_loop(
    "wrap up",
    nil,
    {{
      provider: "mock",
      max_iterations: 2,
      loop_until_done: true,
      session_id: "{session_id}",
      llm_caller: stub_llm,
    }},
  )
  log(result.status)
  log(shared_get(call_counter))
  let reminder_events = transcript_events_by_kind(result.transcript, "system_reminder")
  var saw_audit_reminder = false
  for event in reminder_events {{
    if event?.reminder?.body == "audit trail: agent finished the merge" {{
      saw_audit_reminder = true
    }}
  }}
  log(saw_audit_reminder)
  let checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  var loop_exit_deliveries = 0
  for event in checkpoints {{
    if event?.metadata?.kind == "loop_exit" && (event?.metadata?.delivered ?? 0) >= 1 {{
      loop_exit_deliveries = loop_exit_deliveries + 1
    }}
  }}
  log(loop_exit_deliveries)
}}
"#
    )
}

#[test]
fn audit_only_reminder_lands_in_transcript_but_model_never_sees_it() {
    let raw =
        run_with_bridge(&audit_only_pipeline("audit-only-records", true)).expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    // Exactly one LLM call: the stub fires once, returns ##DONE##, the
    // loop exits. The audit_only reminder DOES NOT trigger an extra
    // iteration — that's the contract harn#2212 makes truthful.
    assert_eq!(
        lines[1], "1",
        "expected exactly one LLM call; lines: {lines:?}"
    );
    // The reminder body shows up in the post-exit transcript audit.
    assert_eq!(
        lines[2], "true",
        "audit_only reminder should land in transcript audit; lines: {lines:?}"
    );
    // The loop_exit checkpoint reports delivered >= 1.
    assert_eq!(
        lines[3], "1",
        "loop_exit checkpoint should report the delivery; lines: {lines:?}"
    );
}

#[test]
fn audit_only_control_run_records_no_audit_reminder() {
    let raw = run_with_bridge(&audit_only_pipeline("audit-only-control", false))
        .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    // Same single LLM call as the audit_only variant — confirming the
    // audit_only path doesn't change the model's view of the run.
    assert_eq!(
        lines[1], "1",
        "expected exactly one LLM call; lines: {lines:?}"
    );
    // No reminder body in the transcript.
    assert_eq!(
        lines[2], "false",
        "control run should not record an audit reminder; lines: {lines:?}"
    );
    // No loop_exit deliveries.
    assert_eq!(
        lines[3], "0",
        "control run should report zero loop_exit deliveries; lines: {lines:?}"
    );
}
