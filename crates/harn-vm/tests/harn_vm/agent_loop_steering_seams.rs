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
//!
//! Steer coverage (rfd/session-inject) — `mode: "steer"` (an alias for
//! `finish_step`) queues a USER-role message via the in-VM
//! `agent_session_push_user_message` primitive (the loop-driver
//! equivalent of the ACP `session/inject` method). Unlike `audit_only`,
//! the steer message is delivered MID-TURN at the next loop checkpoint
//! (a tool boundary / iteration boundary), NOT at `loop_exit`, so the
//! model sees it before its next call. The `steer_*` tests below prove
//! three claims: (a) mid-turn pickup at a tool boundary — a
//! non-`loop_exit` checkpoint reports `delivered >= 1` and NO `loop_exit`
//! checkpoint delivers it; (b) chronological order — the steer user
//! message is spliced AFTER the tool_result and BEFORE the final
//! assistant message in transcript message order; (c) eval-safety — a
//! control variant that does not push the steer produces an identical
//! LLM-call count and tool_result count and no extra user message and no
//! mid-turn delivery.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use harn_vm::bridge::HostBridge;
use harn_vm::value::VmError;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Canonical session journals outlive a VM and may be shared by parallel test
/// processes. Mint a process-local monotonic id for every pipeline invocation
/// so repeated runs cannot rehydrate an earlier transcript without relying on
/// wall-clock time or a random source.
fn fresh_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SESSION_COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

fn run_with_bridge(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let session_store_root = tempfile::tempdir().map_err(|e| e.to_string())?;
    // The scenarios intentionally reuse readable session IDs across tests;
    // isolate their durable journal so repeated local runs cannot rehydrate
    // an earlier transcript and change the message counts under test.
    let session_store_root_path = session_store_root
        .path()
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let source = source.replace("__HARN_TEST_SESSION_STORE_ROOT__", &session_store_root_path);
    let chunk = harn_vm::compile_source(&source)?;
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
  const registry = tool_registry()
  const handler_calls = shared_cell({{scope: "task_group", key: "handler-{session_id}", initial: 0}})
  const tools = tool_define(
    registry,
    "would_force_push",
    "Test stand-in for an irreversible side-effect tool.",
    {{
      parameters: {{}},
      handler: {{ _args ->
        const hsnap = shared_snapshot(handler_calls)
        shared_cas(handler_calls, hsnap, hsnap.value + 1)
        return "would have force-pushed"
      }},
    }},
  )
  const iteration_state = shared_cell({{scope: "task_group", key: "iter-{session_id}", initial: 0}})
  const mock_llm = {{ _call ->
    const snap = shared_snapshot(iteration_state)
    const n = snap.value
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
  const result = agent_loop(
    "do the push",
    nil,
    {{
      provider: "mock",
      tools: tools,
      tool_format: "native",
      root: "__HARN_TEST_SESSION_STORE_ROOT__",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "{session_id}",
      llm_caller: mock_llm,
    }},
  )
  log(result.status)
  const checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  let skipped_count = 0
  for event in checkpoints {{
    if event?.metadata?.dispatch_skipped == true {{
      skipped_count = skipped_count + 1
    }}
  }}
  log(skipped_count)
  const stats = transcript_stats(result.transcript)
  log(stats.tool_result_message_count)
  log(shared_get(handler_calls))
  const messages = transcript_messages(result.transcript)
  let placeholder_count = 0
  for message in messages {{
    const role = message?.role ?? ""
    if role == "tool" || role == "tool_result" {{
      if contains(to_string(message?.content ?? ""), "was not dispatched: interrupted") {{
        placeholder_count = placeholder_count + 1
      }}
    }}
  }}
  log(placeholder_count)
}}
"#
    )
}

#[test]
fn pre_tool_dispatch_skips_when_interrupt_immediate_queued_mid_turn() {
    let raw = run_with_bridge(&race_window_pipeline(
        &fresh_session_id("race-window-stops-dispatch"),
        true,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);
    // Status: done (model returned `##DONE##` on the second iteration).
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    // Exactly one loop_checkpoint event reported dispatch_skipped=true.
    assert_eq!(
        lines[1], "1",
        "expected one skipped dispatch; lines: {lines:?}"
    );
    // Exactly one tool_result message — the synthesized "interrupted"
    // placeholder that closes out the persisted tool_use turn. Without it,
    // the next Anthropic-native LLM call (or a resume that keeps the
    // transcript) is rejected with HTTP 400 "tool_use ids were found
    // without tool_result blocks".
    assert_eq!(
        lines[2], "1",
        "expected the synthesized placeholder tool_result; lines: {lines:?}"
    );
    // The tool handler itself NEVER executed — the placeholder is
    // bookkeeping, not a dispatch.
    assert_eq!(
        lines[3], "0",
        "the tool must not actually run; lines: {lines:?}"
    );
    // And the placeholder self-describes as an interrupted non-dispatch.
    assert_eq!(
        lines[4], "1",
        "placeholder should carry the interrupted marker; lines: {lines:?}"
    );
}

#[test]
fn pre_tool_dispatch_dispatches_when_no_injection_queued() {
    let raw = run_with_bridge(&race_window_pipeline(
        &fresh_session_id("race-window-no-stop"),
        false,
    ))
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
    // The handler really ran exactly once.
    assert_eq!(
        lines[3], "1",
        "the tool handler should run once; lines: {lines:?}"
    );
    // No synthesized placeholder in the control run.
    assert_eq!(
        lines[4], "0",
        "control run must not synthesize placeholders; lines: {lines:?}"
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
  const call_counter = shared_cell(
    {{scope: "task_group", key: "audit-only-llm-calls-{session_id}", initial: 0}},
  )
  const stub_llm = {{ _call ->
    const snap = shared_snapshot(call_counter)
    shared_cas(call_counter, snap, snap.value + 1)
    return {{
      ok: true,
      value: {{text: "all set ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}
  const result = agent_loop(
    "wrap up",
    nil,
    {{
      provider: "mock",
      root: "__HARN_TEST_SESSION_STORE_ROOT__",
      max_iterations: 2,
      loop_until_done: true,
      session_id: "{session_id}",
      llm_caller: stub_llm,
    }},
  )
  log(result.status)
  log(shared_get(call_counter))
  const reminder_events = transcript_events_by_kind(result.transcript, "system_reminder")
  let saw_audit_reminder = false
  for event in reminder_events {{
    if event?.reminder?.body == "audit trail: agent finished the merge" {{
      saw_audit_reminder = true
    }}
  }}
  log(saw_audit_reminder)
  const checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  let loop_exit_deliveries = 0
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
    let raw = run_with_bridge(&audit_only_pipeline(
        &fresh_session_id("audit-only-records"),
        true,
    ))
    .expect("script must run");
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
    let raw = run_with_bridge(&audit_only_pipeline(
        &fresh_session_id("audit-only-control"),
        false,
    ))
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

/// Harn pipeline that exercises the steer path (rfd/session-inject). On
/// iteration 0 the stub LLM caller — BEFORE returning a `would_force_push`
/// tool call — queues a USER-role steer via the in-VM
/// `agent_session_push_user_message` primitive with `mode: "steer"` (the
/// loop-driver equivalent of the ACP `session/inject` method). On
/// iteration 1 the model returns `##DONE##`. The `mode: "steer"` string is
/// passed verbatim through the builtin, which maps `steer` ->
/// `finish_step` (delivered at the next loop checkpoint, i.e. the tool
/// boundary), NOT `loop_exit`.
///
/// The script logs seven fields, identical between the steer and control
/// variants so the no-inject control is directly comparable:
///
///   0. final status
///   1. LLM call count observed by the caller stub
///   2. tool_result message count
///   3. mid-turn deliveries: count of `loop_checkpoint` events whose
///      `kind` is NOT `loop_exit` and whose `delivered` is `>= 1`
///   4. loop_exit deliveries: count of `loop_checkpoint` events whose
///      `kind` IS `loop_exit` and whose `delivered` is `>= 1`
///   5. count of `user`-role transcript messages whose content equals the
///      steer text
///   6. chronological-order verdict — `"ok"` iff the steer user message
///      sits strictly after the tool_result and strictly before the final
///      assistant message in transcript message order; otherwise a
///      diagnostic string `"<toolIdx>/<steerIdx>/<asstIdx>"`.
fn steer_pipeline(session_id: &str, push_steer_mid_turn: bool) -> String {
    let push_line = if push_steer_mid_turn {
        format!(
            r#"      agent_session_push_user_message(
        "{session_id}",
        {{content: "actually use auth_v2.go", mode: "steer"}},
      )"#
        )
    } else {
        String::new()
    };
    format!(
        r#"
import {{ agent_session_push_user_message }} from "std/agent/state"

pipeline main(task) {{
  clear_tool_hooks()
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "would_force_push",
    "Test stand-in for an irreversible side-effect tool.",
    {{parameters: {{}}, handler: {{ _args -> return "would have force-pushed" }}}},
  )
  const iteration_state = shared_cell({{scope: "task_group", key: "steer-iter-{session_id}", initial: 0}})
  const call_counter = shared_cell({{scope: "task_group", key: "steer-calls-{session_id}", initial: 0}})
  const mock_llm = {{ _call ->
    const csnap = shared_snapshot(call_counter)
    shared_cas(call_counter, csnap, csnap.value + 1)
    const snap = shared_snapshot(iteration_state)
    const n = snap.value
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
  const result = agent_loop(
    "do the push",
    nil,
    {{
      provider: "mock",
      tools: tools,
      tool_format: "native",
      root: "__HARN_TEST_SESSION_STORE_ROOT__",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "{session_id}",
      llm_caller: mock_llm,
    }},
  )
  log(result.status)
  log(shared_get(call_counter))
  const stats = transcript_stats(result.transcript)
  log(stats.tool_result_message_count)

  const checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  let mid_turn_deliveries = 0
  let loop_exit_deliveries = 0
  for event in checkpoints {{
    const delivered = event?.metadata?.delivered ?? 0
    if delivered >= 1 {{
      if event?.metadata?.kind == "loop_exit" {{
        loop_exit_deliveries = loop_exit_deliveries + 1
      }} else {{
        mid_turn_deliveries = mid_turn_deliveries + 1
      }}
    }}
  }}
  log(mid_turn_deliveries)
  log(loop_exit_deliveries)

  // Walk the transcript messages IN ORDER to prove chronological splice:
  // the steer user message must land after the tool_result and before the
  // final assistant message.
  const messages = transcript_messages(result.transcript)
  let steer_user_count = 0
  let tool_result_idx = -1
  let steer_idx = -1
  let last_assistant_idx = -1
  let idx = 0
  for message in messages {{
    const role = message?.role ?? ""
    const content = message?.content ?? ""
    if (role == "tool_result" || role == "tool") && tool_result_idx == -1 {{
      tool_result_idx = idx
    }}
    if role == "user" && content == "actually use auth_v2.go" {{
      steer_user_count = steer_user_count + 1
      if steer_idx == -1 {{
        steer_idx = idx
      }}
    }}
    if role == "assistant" {{
      last_assistant_idx = idx
    }}
    idx = idx + 1
  }}
  log(steer_user_count)
  if tool_result_idx >= 0 && steer_idx > tool_result_idx && last_assistant_idx > steer_idx {{
    log("ok")
  }} else {{
    log("${{tool_result_idx}}/${{steer_idx}}/${{last_assistant_idx}}")
  }}
}}
"#
    )
}

#[test]
fn steer_user_message_delivered_mid_turn_at_tool_boundary() {
    let raw = run_with_bridge(&steer_pipeline(&fresh_session_id("steer-mid-turn"), true))
        .expect("script must run");
    let lines = out_lines(&raw);
    // Status: done (model returned `##DONE##` on the second iteration).
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    // Two LLM calls: iteration 0 emits the tool call (after queuing the
    // steer), iteration 1 sees the spliced steer + tool_result and
    // returns ##DONE##.
    assert_eq!(lines[1], "2", "expected two LLM calls; lines: {lines:?}");
    // The tool dispatched once.
    assert_eq!(
        lines[2], "1",
        "expected one tool dispatch; lines: {lines:?}"
    );
    // (a) MID-TURN PICKUP AT TOOL BOUNDARY: a non-`loop_exit` checkpoint
    // (post_tool_dispatch / iteration_start of iteration 1) reported the
    // steer delivery during the running turn.
    assert!(
        lines[3].parse::<i64>().unwrap_or(0) >= 1,
        "expected >= 1 mid-turn delivery at a tool boundary; lines: {lines:?}"
    );
    // NO `loop_exit` checkpoint delivered the steer — this distinguishes
    // steer (finish_step) from audit_only/queue.
    assert_eq!(
        lines[4], "0",
        "steer must NOT be delivered at loop_exit; lines: {lines:?}"
    );
    // The steer user message landed in the transcript exactly once.
    assert_eq!(
        lines[5], "1",
        "expected exactly one steer user message in the transcript; lines: {lines:?}"
    );
    // (b) CHRONOLOGICAL ORDER: tool_result_idx < steer_idx <
    // last_assistant_idx, proven by walking transcript_messages in order.
    assert_eq!(
        lines[6], "ok",
        "steer user message must sit chronologically after the tool_result \
         and before the final assistant message (verdict is \
         tool_result_idx/steer_idx/last_assistant_idx on failure); lines: {lines:?}"
    );
}

#[test]
fn steer_control_run_without_inject_is_eval_safe() {
    let raw = run_with_bridge(&steer_pipeline(&fresh_session_id("steer-control"), false))
        .expect("script must run");
    let lines = out_lines(&raw);
    // (c) NO-INJECT PATH UNCHANGED: same status, same LLM-call count, and
    // same tool_result count as the steer variant — the no-inject path is
    // byte-identical from the loop's perspective (eval-safe).
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "2",
        "control must make the same two LLM calls as the steer variant; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "1",
        "control must dispatch the tool exactly once, same as the steer variant; lines: {lines:?}"
    );
    // No mid-turn deliveries and no loop_exit deliveries — nothing was
    // queued.
    assert_eq!(
        lines[3], "0",
        "control run should report zero mid-turn deliveries; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "0",
        "control run should report zero loop_exit deliveries; lines: {lines:?}"
    );
    // No steer user message in the transcript.
    assert_eq!(
        lines[5], "0",
        "control run should not record a steer user message; lines: {lines:?}"
    );
    // No steer message exists, so the ordering verdict is the diagnostic
    // form (steer_idx stays -1) — confirming the control differs from the
    // steer variant only by the absence of the steer message.
    assert_ne!(
        lines[6], "ok",
        "control run has no steer message to order; lines: {lines:?}"
    );
}
