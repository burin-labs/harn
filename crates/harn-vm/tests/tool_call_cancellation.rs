#![recursion_limit = "256"]
//! Integration coverage for `cancel_in_flight_tool_call` (harn#2213).
//!
//! Verifies the full dispatch path: a Harn pipeline registers a slow
//! tool, the agent loop dispatches it, a spawned task triggers
//! `cancel_in_flight_tool_call`, and the dispatched tool's result is
//! shaped as `status: "cancelled"`.

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

/// Mid-flight cancellation: tool is sleeping; a spawned task issues
/// `cancel_in_flight_tool_call`; tool result comes back as
/// `status: "cancelled"`. The agent loop continues past the cancelled
/// call without tearing down the session — that's the distinguishing
/// property versus `session/cancel`.
#[test]
fn cancel_in_flight_tool_call_overrides_dispatch_with_cancelled_result() {
    let source = r#"
pipeline main(_) {
  clear_tool_hooks()
  let registry = tool_registry()
  let tools = tool_define(
    registry,
    "slow_tool",
    "A tool that sleeps long enough that we can cancel mid-flight.",
    {parameters: {}, handler: { _args ->
      sleep_ms(5000)
      return "should not arrive"
    }},
  )
  let counter = shared_cell({scope: "task_group", key: "llm-cancel-test", initial: 0})
  let mock_llm = { _call ->
    let snap = shared_snapshot(counter)
    shared_cas(counter, snap, snap.value + 1)
    if snap.value == 0 {
      return {
        ok: true,
        value: {
          text: "",
          tool_calls: [{id: "slow_call_1", name: "slow_tool", arguments: {}}],
          provider: "mock",
          model: "mock",
        },
      }
    }
    return {
      ok: true,
      value: {text: "ok ##DONE##", tool_calls: [], provider: "mock", model: "mock"},
    }
  }
  let canceller = spawn {
    sleep_ms(100)
    cancel_in_flight_tool_call(
      "tcc-test-session",
      "slow_call_1",
      {reason: "test cancel mid-flight", inject_reminder: false},
    )
  }
  let result = agent_loop(
    "do slow work",
    nil,
    {
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "tcc-test-session",
      llm_caller: mock_llm,
    },
  )
  let cancel_outcome = await(canceller)
  log(result.status)
  log(cancel_outcome.status)
  log(cancel_outcome.tool ?? "<no tool>")
  // The tool_result message recorded in the transcript renders the
  // observation text from the cancellation result. Verify the
  // "cancelled call to slow_tool" stamp made it in.
  let messages = transcript_messages(result.transcript)
  let saw_cancellation_observation = false
  for msg in messages {
    let role = to_string(msg?.role ?? "")
    if role != "tool" && role != "tool_result" {
      continue
    }
    let content = to_string(msg?.content ?? "")
    if contains(content, "cancelled call to slow_tool") {
      saw_cancellation_observation = true
    }
  }
  log(saw_cancellation_observation ? "observation_seen" : "observation_missing")
}
"#;
    let raw = run_with_bridge(source).expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "agent loop status; lines: {lines:?}");
    assert_eq!(
        lines[1], "cancelled",
        "cancel_in_flight_tool_call status; lines: {lines:?}"
    );
    assert_eq!(lines[2], "slow_tool", "tool name; lines: {lines:?}");
    assert_eq!(
        lines[3], "observation_seen",
        "cancellation observation in transcript; lines: {lines:?}"
    );
}

/// `cancel_in_flight_tool_call` on an unknown id returns `not_found`
/// — the caller can distinguish "never started" from "already cancelled".
#[test]
fn cancel_in_flight_tool_call_returns_not_found_for_unknown_call_id() {
    let source = r#"
pipeline main(_) {
  let outcome = cancel_in_flight_tool_call(
    "no-such-session",
    "no-such-call",
    {reason: "missing", inject_reminder: false, timeout_ms: 0},
  )
  log(outcome.status)
  log(outcome.tool ?? "<nil>")
  log(outcome.call_id)
}
"#;
    let raw = run_with_bridge(source).expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "not_found", "status; lines: {lines:?}");
    assert_eq!(
        lines[1], "<nil>",
        "tool nil for not_found; lines: {lines:?}"
    );
    assert_eq!(lines[2], "no-such-call", "call_id echoed; lines: {lines:?}");
}

/// Twice-cancelled call: the second invocation reports
/// `already_cancelled` so the host can suppress redundant work.
#[test]
fn cancel_in_flight_tool_call_returns_already_cancelled_on_repeat() {
    let source = r#"
pipeline main(_) {
  clear_tool_hooks()
  let registry = tool_registry()
  let tools = tool_define(
    registry,
    "slow_tool",
    "A tool that sleeps long enough that we can cancel mid-flight.",
    {parameters: {}, handler: { _args ->
      sleep_ms(5000)
      return "should not arrive"
    }},
  )
  let counter = shared_cell({scope: "task_group", key: "llm-cancel-repeat", initial: 0})
  let mock_llm = { _call ->
    let snap = shared_snapshot(counter)
    shared_cas(counter, snap, snap.value + 1)
    if snap.value == 0 {
      return {
        ok: true,
        value: {
          text: "",
          tool_calls: [{id: "slow_call_repeat", name: "slow_tool", arguments: {}}],
          provider: "mock",
          model: "mock",
        },
      }
    }
    return {
      ok: true,
      value: {text: "ok ##DONE##", tool_calls: [], provider: "mock", model: "mock"},
    }
  }
  let canceller = spawn {
    sleep_ms(100)
    let first = cancel_in_flight_tool_call(
      "tcc-test-session-repeat",
      "slow_call_repeat",
      {reason: "first cancel", inject_reminder: false, timeout_ms: 0},
    )
    let second = cancel_in_flight_tool_call(
      "tcc-test-session-repeat",
      "slow_call_repeat",
      {reason: "second cancel", inject_reminder: false, timeout_ms: 0},
    )
    return [first.status, second.status]
  }
  let result = agent_loop(
    "do slow work",
    nil,
    {
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "tcc-test-session-repeat",
      llm_caller: mock_llm,
    },
  )
  let statuses = await(canceller)
  log(result.status)
  log(statuses[0])
  log(statuses[1])
}
"#;
    let raw = run_with_bridge(source).expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "agent loop status; lines: {lines:?}");
    assert_eq!(lines[1], "cancelled", "first cancel; lines: {lines:?}");
    assert_eq!(
        lines[2], "already_cancelled",
        "second cancel; lines: {lines:?}"
    );
}

/// `inject_reminder: true` pushes a system reminder via the bridge so
/// the model knows it was stopped on the host's behalf and doesn't
/// immediately retry the same call.
#[test]
fn cancel_in_flight_tool_call_pushes_reminder_when_requested() {
    use harn_vm::bridge::DeliveryCheckpoint;

    harn_vm::reset_thread_local_state();
    let source = r#"
pipeline main(_) {
  clear_tool_hooks()
  let registry = tool_registry()
  let tools = tool_define(
    registry,
    "slow_tool",
    "A tool that sleeps long enough that we can cancel mid-flight.",
    {parameters: {}, handler: { _args ->
      sleep_ms(5000)
      return "should not arrive"
    }},
  )
  let counter = shared_cell({scope: "task_group", key: "llm-cancel-reminder", initial: 0})
  let mock_llm = { _call ->
    let snap = shared_snapshot(counter)
    shared_cas(counter, snap, snap.value + 1)
    if snap.value == 0 {
      return {
        ok: true,
        value: {
          text: "",
          tool_calls: [{id: "slow_call_rem", name: "slow_tool", arguments: {}}],
          provider: "mock",
          model: "mock",
        },
      }
    }
    return {
      ok: true,
      value: {text: "ok ##DONE##", tool_calls: [], provider: "mock", model: "mock"},
    }
  }
  let canceller = spawn {
    sleep_ms(100)
    cancel_in_flight_tool_call(
      "tcc-test-session-reminder",
      "slow_call_rem",
      {reason: "user clicked stop", inject_reminder: true, timeout_ms: 0},
    )
  }
  let result = agent_loop(
    "do slow work",
    nil,
    {
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "tcc-test-session-reminder",
      llm_caller: mock_llm,
    },
  )
  let _ = await(canceller)
  log(result.status)
  let reminders = transcript_events_by_kind(result.transcript, "system_reminder")
  let saw_cancellation = false
  for event in reminders {
    if contains(event?.reminder?.body ?? "", "cancelled by the host") {
      saw_cancellation = true
    }
  }
  log(saw_cancellation)
}
"#;
    let chunk = harn_vm::compile_source(source).expect("compile");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let (output, bridge) = rt.block_on(async {
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
                let _ = vm.execute(&chunk).await.expect("execute");
                harn_vm::llm::clear_current_host_bridge();
                (vm.output().to_string(), bridge)
            })
            .await
    });
    let lines = out_lines(&output);
    assert_eq!(lines[0], "done", "loop status; lines: {lines:?}");
    // We allow `saw_cancellation` to be either true (reminder drained
    // into transcript) or false (reminder still queued on the bridge),
    // because rendering depends on whether the loop's next iteration
    // drained the interrupt_immediate slot. The next assertion proves
    // the reminder was at least queued.

    // Drain whatever is left on the bridge — we expect the cancellation
    // reminder to have been queued via `inject_reminder: true`.
    let drained = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                bridge
                    .take_queued_transcript_injections_for(DeliveryCheckpoint::InterruptImmediate)
                    .await
            })
            .await
    });
    let saw_in_queue = drained.iter().any(|inj| {
        matches!(
            inj,
            harn_vm::bridge::QueuedTranscriptInjection::Reminder(rem)
                if rem.reminder.body.contains("cancelled by the host")
        )
    });
    let saw_in_transcript = lines.get(1).map(String::as_str) == Some("true");
    assert!(
        saw_in_queue || saw_in_transcript,
        "cancellation reminder should land either in the transcript or in the bridge queue; lines: {lines:?}, drained_len={}",
        drained.len()
    );
}
