//! Integration coverage for the typed `agent_stage` seam catalog
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
    let source = format!("import {{ agent_loop }} from \"std/agent/loop\"\n{source}");
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

/// Harn pipeline that exercises `pre_tool_dispatch`. The stub LLM caller can
/// queue either an `interrupt_immediate` reminder or user message before
/// returning the tool call, simulating a host that pressed "STOP" mid-turn.
/// The no-injection variant proves the same tool dispatches normally.
#[derive(Clone, Copy)]
enum RaceWindowInjection {
    None,
    Reminder,
    User,
}

fn race_window_pipeline(session_id: &str, injection: RaceWindowInjection) -> String {
    let push_line = match injection {
        RaceWindowInjection::Reminder => format!(
            r#"      harness.agent.session_push_bridge_injection(
        "{session_id}",
        {{body: "STOP — abort the push", mode: "interrupt_immediate", role_hint: "system"}},
      )"#
        ),
        RaceWindowInjection::User => format!(
            r#"      harness.agent.session_push_user_message(
        "{session_id}",
        {{content: "STOP — abort the push", mode: "interrupt_immediate"}},
      )"#
        ),
        RaceWindowInjection::None => String::new(),
    };
    format!(
        r#"
import {{ agent_session_push_bridge_injection }} from "std/agent/state"

pipeline main(harness: Harness, task: unknown) {{
  harness.tools.clear_hooks()
  const registry = tool_registry()
  const handler_calls = harness.runtime.shared_cell({{scope: "task_group", key: "handler-{session_id}", initial: 0}})
  const tools = tool_define(
    registry,
    "would_force_push",
    "Test stand-in for an irreversible side-effect tool.",
    {{
      parameters: {{}},
      handler: {{ _args ->
        const hsnap = harness.runtime.shared_snapshot(handler_calls)
        harness.runtime.shared_cas(handler_calls, hsnap, hsnap.value + 1)
        return "would have force-pushed"
      }},
    }},
  )
  const iteration_state = harness.runtime.shared_cell({{scope: "task_group", key: "iter-{session_id}", initial: 0}})
  const mock_llm = {{ _call ->
    const snap = harness.runtime.shared_snapshot(iteration_state)
    const n = snap.value
    harness.runtime.shared_cas(iteration_state, snap, n + 1)
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
    harness,
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
  harness.stdio.log(result.status)
  const checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  let skipped_count = 0
  for event in checkpoints {{
    if event?.metadata?.dispatch_skipped == true {{
      skipped_count = skipped_count + 1
    }}
  }}
  harness.stdio.log(skipped_count)
  const stats = transcript_stats(result.transcript)
  harness.stdio.log(stats.tool_result_message_count)
  harness.stdio.log(harness.runtime.shared_get(handler_calls))
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
  harness.stdio.log(placeholder_count)
  let interrupt_user_count = 0
  for message in messages {{
    if message?.role == "user" && message?.content == "STOP — abort the push" {{
      interrupt_user_count = interrupt_user_count + 1
    }}
  }}
  harness.stdio.log(interrupt_user_count)
  const reminder_events = transcript_events_by_kind(result.transcript, "system_reminder")
  let stop_reminder_count = 0
  for event in reminder_events {{
    if event?.reminder?.body == "STOP — abort the push" {{
      stop_reminder_count = stop_reminder_count + 1
    }}
  }}
  harness.stdio.log(stop_reminder_count)
}}
"#
    )
}

#[test]
fn pre_tool_dispatch_skips_when_interrupt_immediate_queued_mid_turn() {
    let raw = run_with_bridge(&race_window_pipeline(
        &fresh_session_id("race-window-stops-dispatch"),
        RaceWindowInjection::Reminder,
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
    assert_eq!(
        lines[5], "0",
        "reminders must not become user turns; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "1",
        "expected one reminder event; lines: {lines:?}"
    );
}

#[test]
fn pre_tool_dispatch_skips_for_interrupt_immediate_user_message() {
    let raw = run_with_bridge(&race_window_pipeline(
        &fresh_session_id("race-window-user-stops-dispatch"),
        RaceWindowInjection::User,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);

    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "expected one skipped dispatch; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "1",
        "expected one placeholder result; lines: {lines:?}"
    );
    assert_eq!(lines[3], "0", "the tool must not run; lines: {lines:?}");
    assert_eq!(
        lines[4], "1",
        "expected interrupted placeholder; lines: {lines:?}"
    );
    assert_eq!(
        lines[5], "1",
        "expected one user-role interrupt; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "0",
        "user input must not become a reminder; lines: {lines:?}"
    );
}

#[test]
fn pre_tool_dispatch_dispatches_when_no_injection_queued() {
    let raw = run_with_bridge(&race_window_pipeline(
        &fresh_session_id("race-window-no-stop"),
        RaceWindowInjection::None,
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
    assert_eq!(
        lines[5], "0",
        "control must not add a user interrupt; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "0",
        "control must not add a reminder; lines: {lines:?}"
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
            r#"  harness.agent.session_push_bridge_injection(
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

pipeline main(harness: Harness, task: unknown) {{
  harness.tools.clear_hooks()
{push_block}
  const call_counter = harness.runtime.shared_cell(
    {{scope: "task_group", key: "audit-only-llm-calls-{session_id}", initial: 0}},
  )
  const stub_llm = {{ _call ->
    const snap = harness.runtime.shared_snapshot(call_counter)
    harness.runtime.shared_cas(call_counter, snap, snap.value + 1)
    return {{
      ok: true,
      value: {{text: "all set ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}
  const result = agent_loop(
    harness,
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
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(call_counter))
  const reminder_events = transcript_events_by_kind(result.transcript, "system_reminder")
  let saw_audit_reminder = false
  for event in reminder_events {{
    if event?.reminder?.body == "audit trail: agent finished the merge" {{
      saw_audit_reminder = true
    }}
  }}
  harness.stdio.log(saw_audit_reminder)
  const checkpoints = transcript_events_by_kind(result.transcript, "loop_checkpoint")
  let loop_exit_deliveries = 0
  for event in checkpoints {{
    if event?.metadata?.kind == "loop_exit" && (event?.metadata?.delivered ?? 0) >= 1 {{
      loop_exit_deliveries = loop_exit_deliveries + 1
    }}
  }}
  harness.stdio.log(loop_exit_deliveries)
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
            r#"      harness.agent.session_push_user_message(
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

pipeline main(harness: Harness, task: unknown) {{
  harness.tools.clear_hooks()
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "would_force_push",
    "Test stand-in for an irreversible side-effect tool.",
    {{parameters: {{}}, handler: {{ _args -> return "would have force-pushed" }}}},
  )
  const iteration_state = harness.runtime.shared_cell({{scope: "task_group", key: "steer-iter-{session_id}", initial: 0}})
  const call_counter = harness.runtime.shared_cell({{scope: "task_group", key: "steer-calls-{session_id}", initial: 0}})
  const mock_llm = {{ _call ->
    const csnap = harness.runtime.shared_snapshot(call_counter)
    harness.runtime.shared_cas(call_counter, csnap, csnap.value + 1)
    const snap = harness.runtime.shared_snapshot(iteration_state)
    const n = snap.value
    harness.runtime.shared_cas(iteration_state, snap, n + 1)
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
    harness,
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
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(call_counter))
  const stats = transcript_stats(result.transcript)
  harness.stdio.log(stats.tool_result_message_count)

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
  harness.stdio.log(mid_turn_deliveries)
  harness.stdio.log(loop_exit_deliveries)

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
  harness.stdio.log(steer_user_count)
  if tool_result_idx >= 0 && steer_idx > tool_result_idx && last_assistant_idx > steer_idx {{
    harness.stdio.log("ok")
  }} else {{
    harness.stdio.log("${{tool_result_idx}}/${{steer_idx}}/${{last_assistant_idx}}")
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

/// Harn pipeline for the completion-judge obligation seam.
///
/// The founder dogfood repro (2026-09-01): the original task asked for an
/// implementation, a test, and a changelog; the user then steered mid-run —
/// "if no typed code exists, say so and stop"; the agent complied and reverted;
/// the completion judge, reading only the frozen session task, vetoed the stop
/// and ordered the withdrawn work redone.
///
/// This drives the real path end to end — a bridge steer, the real
/// `finish_step` drain, the typed `injectedMode` marker the drain stamps, the
/// obligations derived at the judge payload seam — and reports what the judge's
/// ACTUAL system prompt contained. The paired control pushes no steer.
fn steer_obligations_pipeline(session_id: &str, push_steer_mid_turn: bool) -> String {
    let push_line = if push_steer_mid_turn {
        r#"          agent_session_push_user_message(
            harness.agent,
            session,
            {content: STEER, mode: "steer"},
          )"#
    } else {
        ""
    };
    format!(
        r###"
import {{ agent_session_push_user_message }} from "std/agent/state"
import {{ llm_text, with_llm_script }} from "std/testing"

const STEER = "Do not match the detail string with ==. If no typed code exists, say so and stop."

pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(
    harness.llm,
    [
      llm_text("I will compare the detail string with == and add the test."),
      llm_text("Reverted the implementation, test, and changelog. No typed code exists, so I am reporting that and stopping as asked. ##DONE##"),
      {{text: "{{\"verdict\":\"done\",\"detail\":\"The run answered the steered question.\"}}"}},
    ],
    {{ ->
      const session = "{session_id}"
      const seen = harness.runtime.shared_cell(
        {{scope: "task_group", key: "obl-closure-{session_id}", initial: 0}},
      )
      const result = agent_loop(
        harness,
        "Implement one bounded improvement, add a focused test, and add a changelog fragment.",
        nil,
        {{
          provider: "mock",
          session_id: session,
          root: "__HARN_TEST_SESSION_STORE_ROOT__",
          loop_until_done: true,
          done_sentinel: "##DONE##",
          max_iterations: 4,
          verify_completion_judge: {{provider: "mock", max_invocations: 2}},
          verify_completion: {{ info ->
            const steers = info?.obligations?.steers ?? []
            harness.runtime.shared_set(seen, len(steers))
            return nil
          }},
          post_turn_callback: {{ info ->
            if info.iteration == 0 {{
{push_line}
            }}
            return nil
          }},
        }},
      )
      harness.stdio.log(result.status)

      // Only the judge's own call carries the stable completion prefix.
      const judged = harness.llm.mock_calls()
        .filter({{ call -> contains(to_string(call?.system ?? ""), "Stable completion goal") }})
        .to_list()
      harness.stdio.log(len(judged))
      if len(judged) == 0 {{
        // Absence must not read as success: say so instead of reporting a
        // prompt verdict nobody measured.
        harness.stdio.log("no_judge_call")
      }} else {{
        const prefix = to_string(judged[0].system)
        if contains(prefix, "Accepted user steering")
          && contains(prefix, "say so and stop")
          && contains(prefix, "supersedes any conflicting earlier requirement") {{
          harness.stdio.log("steer_in_judge_prompt")
        }} else {{
          if contains(prefix, "Accepted user steering") {{
            harness.stdio.log("steering_block_incomplete")
          }} else {{
            harness.stdio.log("no_steering_block")
          }}
        }}
      }}
      harness.stdio.log(harness.runtime.shared_snapshot(seen).value)
    }},
  )
}}
"###
    )
}

/// RED ON MAIN: the judge prompt carries only the frozen session task, so the
/// steer is invisible to the authority that decides whether the run may stop.
#[test]
fn accepted_steer_updates_the_completion_judge_obligations() {
    let raw = run_with_bridge(&steer_obligations_pipeline(
        &fresh_session_id("steer-obligations"),
        true,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "expected exactly one completion-judge call; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "steer_in_judge_prompt",
        "the judge's own system prompt must carry the accepted steer AND its \
         supersession framing; lines: {lines:?}"
    );
    // The deterministic exit authority reads the SAME derived obligations.
    assert_eq!(
        lines[3], "1",
        "the verify_completion closure must see the derived steer; lines: {lines:?}"
    );
}

/// NEGATIVE CONTROL: with no steer the judge prompt renders no steering block
/// at all, so an unsteered run's prefix is byte-identical to what it was before
/// this seam existed and the judge's authority is untouched.
#[test]
fn unsteered_run_renders_no_steering_block() {
    let raw = run_with_bridge(&steer_obligations_pipeline(
        &fresh_session_id("steer-obligations-control"),
        false,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "control must make the same single judge call; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "no_steering_block",
        "an unsteered run must render no steering block; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "0",
        "control must derive zero steers; lines: {lines:?}"
    );
}

/// Provider-free real-loop coverage for goal retargeting. The first model call
/// runs under ALPHA. After that call, the loop callback optionally queues a
/// bridge message; the next loop checkpoint drains it before call two. The
/// caller records the actual system prompt it received on both calls, while
/// the result transcript supplies the replay-visible transition receipt.
#[derive(Clone, Copy)]
enum GoalRetargetInjection {
    None,
    Steer,
    AuditOnly,
    RepeatedId,
}

fn goal_retarget_pipeline(session_id: &str, injection: GoalRetargetInjection) -> String {
    let injection_block = match injection {
        GoalRetargetInjection::None => String::new(),
        GoalRetargetInjection::Steer => r#"          agent_session_push_user_message(
            harness.agent,
            session,
            {content: BRAVO, mode: "steer"},
          )"#
        .to_string(),
        GoalRetargetInjection::AuditOnly => r#"          agent_session_push_user_message(
            harness.agent,
            session,
            {content: BRAVO, mode: "audit_only"},
          )"#
        .to_string(),
        GoalRetargetInjection::RepeatedId => {
            r#"          const repeated_id = agent_session_push_user_message(
            harness.agent,
            session,
            {content: BRAVO, mode: "steer"},
          )
          // Reproduce replay/import duplication without inventing a second
          // goal-sync path: both delivered turns carry the bridge's real id.
          harness.agent.inject(
            session,
            {
              role: "user",
              content: BRAVO,
              messageId: repeated_id,
              injectedMode: "finish_step",
            },
          )"#
            .to_string()
        }
    };
    format!(
        r###"
import {{ goal, with_goal }} from "std/agent/goal"
import {{ agent_session_push_user_message }} from "std/agent/state"

const ALPHA = "ALPHA_GOAL_ONLY_7580"
const BRAVO = "BRAVO_GOAL_ONLY_7580"
const STABLE = "STABLE_SYSTEM_ONLY_7580"

pipeline main(harness: Harness, task: unknown) {{
  harness.tools.clear_hooks()
  const session = "{session_id}"
  const calls = harness.runtime.shared_cell(
    {{scope: "task_group", key: "goal-retarget-calls-{session_id}", initial: 0}},
  )
  const first_system = harness.runtime.shared_cell(
    {{scope: "task_group", key: "goal-retarget-first-{session_id}", initial: ""}},
  )
  const second_system = harness.runtime.shared_cell(
    {{scope: "task_group", key: "goal-retarget-second-{session_id}", initial: ""}},
  )
  const mock_llm = {{ call ->
    const snap = harness.runtime.shared_snapshot(calls)
    const n = snap.value
    harness.runtime.shared_cas(calls, snap, n + 1)
    const system = to_string(call?.system ?? "")
    if n == 0 {{
      harness.runtime.shared_set(first_system, system)
      return {{
        ok: true,
        value: {{text: "Still working.", tool_calls: [], provider: "mock", model: "mock"}},
      }}
    }}
    harness.runtime.shared_set(second_system, system)
    return {{
      ok: true,
      value: {{text: "Retargeted work complete. ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}
  const options = with_goal(
    {{
      provider: "mock",
      session_id: session,
      root: "__HARN_TEST_SESSION_STORE_ROOT__",
      loop_until_done: true,
      done_sentinel: "##DONE##",
      max_iterations: 3,
      llm_caller: mock_llm,
      post_turn_callback: {{ info ->
        if info.iteration == 0 {{
{injection_block}
        }}
        return nil
      }},
    }},
    goal({{
      objective: ALPHA,
      success_criteria: [{{id: "alpha", description: "Finish ALPHA"}}],
      constraints: ["Stay on ALPHA"],
    }}),
  )
  const result = agent_loop(harness, "Follow the current typed goal.", STABLE, options)
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(calls))

  const first = harness.runtime.shared_get(first_system)
  const second = harness.runtime.shared_get(second_system)
  harness.stdio.log(
    if contains(first, STABLE) && contains(first, ALPHA) && !contains(first, BRAVO) {{
      "alpha_only"
    }} else {{
      "wrong"
    }},
  )
  harness.stdio.log(
    if contains(second, STABLE) && contains(second, BRAVO) && !contains(second, ALPHA) {{
      "bravo_only"
    }} else {{
      if contains(second, ALPHA) && !contains(second, BRAVO) {{
        "alpha_only"
      }} else {{
        if contains(second, ALPHA) && contains(second, BRAVO) {{ "both" }} else {{ "neither" }}
      }}
    }},
  )

  const receipts = transcript_events_by_kind(result.transcript, "typed_checkpoint")
    .filter({{ event -> event?.metadata?.schema == "harn.goal_transition.v1" }})
    .to_list()
  harness.stdio.log(len(receipts))
  if len(receipts) == 0 {{
    harness.stdio.log("none")
    harness.stdio.log("none")
  }} else {{
    harness.stdio.log(receipts[0].metadata.previous_goal.objective)
    harness.stdio.log(receipts[0].metadata.next_goal.objective)
  }}

  let bravo_user_messages = 0
  for message in transcript_messages(result.transcript) {{
    if message?.role == "user" && message?.content == BRAVO {{
      bravo_user_messages = bravo_user_messages + 1
    }}
  }}
  harness.stdio.log(bravo_user_messages)
}}
"###
    )
}

#[test]
fn accepted_steer_retargets_the_next_real_loop_model_call_once() {
    let raw = run_with_bridge(&goal_retarget_pipeline(
        &fresh_session_id("goal-retarget-steer"),
        GoalRetargetInjection::Steer,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);

    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(lines[1], "2", "expected two model calls; lines: {lines:?}");
    assert_eq!(
        lines[2], "alpha_only",
        "call one must see only the original goal; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "bravo_only",
        "the very next call must see BRAVO with stale ALPHA retired; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "1",
        "one accepted steer must emit exactly one goal transition; lines: {lines:?}"
    );
    assert_eq!(lines[5], "ALPHA_GOAL_ONLY_7580", "lines: {lines:?}");
    assert_eq!(lines[6], "BRAVO_GOAL_ONLY_7580", "lines: {lines:?}");
    assert_eq!(
        lines[7], "1",
        "the bridge steer must land as exactly one user turn; lines: {lines:?}"
    );
}

#[test]
fn no_steer_keeps_the_original_goal_and_emits_no_transition() {
    let raw = run_with_bridge(&goal_retarget_pipeline(
        &fresh_session_id("goal-retarget-control"),
        GoalRetargetInjection::None,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);

    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(lines[1], "2", "expected two model calls; lines: {lines:?}");
    assert_eq!(lines[2], "alpha_only", "lines: {lines:?}");
    assert_eq!(lines[3], "alpha_only", "lines: {lines:?}");
    assert_eq!(lines[4], "0", "lines: {lines:?}");
    assert_eq!(lines[5], "none", "lines: {lines:?}");
    assert_eq!(lines[6], "none", "lines: {lines:?}");
    assert_eq!(lines[7], "0", "lines: {lines:?}");
}

#[test]
fn audit_only_message_never_retargets_a_model_call() {
    let raw = run_with_bridge(&goal_retarget_pipeline(
        &fresh_session_id("goal-retarget-audit-only"),
        GoalRetargetInjection::AuditOnly,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);

    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(lines[1], "2", "expected two model calls; lines: {lines:?}");
    assert_eq!(lines[2], "alpha_only", "lines: {lines:?}");
    assert_eq!(
        lines[3], "alpha_only",
        "audit-only input must not alter the next model goal; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "0",
        "audit-only input must not emit a goal transition; lines: {lines:?}"
    );
    assert_eq!(
        lines[7], "1",
        "the control must prove the audit-only user turn landed; lines: {lines:?}"
    );
}

#[test]
fn repeated_message_id_emits_one_transition_and_one_retarget() {
    let raw = run_with_bridge(&goal_retarget_pipeline(
        &fresh_session_id("goal-retarget-repeated-id"),
        GoalRetargetInjection::RepeatedId,
    ))
    .expect("script must run");
    let lines = out_lines(&raw);

    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(lines[1], "2", "expected two model calls; lines: {lines:?}");
    assert_eq!(lines[2], "alpha_only", "lines: {lines:?}");
    assert_eq!(lines[3], "bravo_only", "lines: {lines:?}");
    assert_eq!(
        lines[4], "1",
        "a repeated delivered message id must transition exactly once; lines: {lines:?}"
    );
    assert_eq!(lines[5], "ALPHA_GOAL_ONLY_7580", "lines: {lines:?}");
    assert_eq!(lines[6], "BRAVO_GOAL_ONLY_7580", "lines: {lines:?}");
    assert_eq!(
        lines[7], "2",
        "the duplicate-id control must actually carry two delivered turns; lines: {lines:?}"
    );
}
