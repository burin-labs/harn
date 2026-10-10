//! A tool call stopped mid-handler reports what it already wrote (harn#9611).
//!
//! The call never returns a result, so its terminal update used to say
//! `mutationStatus: unknown` even after the handler's write had landed. The
//! runtime records each completed write against the dispatching call, and a
//! stopped call reports `applied` with those paths. A call that wrote nothing
//! still says `unknown`: it may have acted through a channel the runtime does
//! not observe. The loop-exit (session cancel) path is covered beside
//! `fire_session_end_hooks`; this drives the real dispatch path end to end.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use harn_vm::agent_events::{AgentEvent, AgentEventSink, ToolCallStatus, ToolMutationStatus};
use harn_vm::bridge::HostBridge;

/// `(tool_call_id, status, mutation_status, changed_paths)` of one update.
type ObservedUpdate = (
    String,
    ToolCallStatus,
    ToolMutationStatus,
    Option<Vec<String>>,
);

#[derive(Default)]
struct UpdateSink {
    updates: Mutex<Vec<ObservedUpdate>>,
}

impl AgentEventSink for UpdateSink {
    fn handle_event(&self, event: &AgentEvent) {
        if let AgentEvent::ToolCallUpdate {
            tool_call_id,
            status,
            mutation_status,
            changed_paths,
            ..
        } = event
        {
            self.updates.lock().unwrap().push((
                tool_call_id.clone(),
                *status,
                *mutation_status,
                changed_paths.clone(),
            ));
        }
    }
}

/// Run one turn whose only tool call writes (or not) and then blocks until
/// `cancel_in_flight_tool_call` stops it. Returns the call's terminal update.
fn stopped_call_update(
    session: &str,
    writes: bool,
) -> (ToolCallStatus, ToolMutationStatus, Option<Vec<String>>) {
    let write = if writes {
        r#"harness.fs.write_text(path_join(harness.fs.workspace_temp_dir(), "stopped-write.txt"), "applied before the stop")"#
    } else {
        ""
    };
    let canceller = format!(
        r#"const canceller = spawn {{
    harness.clock.sleep_ms(200)
    harness.agent.cancel_in_flight_tool_call("{session}", "stopped_call", {{reason: "person stopped", inject_reminder: false}})
  }}"#
    );
    let source = format!(
        r#"import {{ agent_loop }} from "std/agent/loop"
pipeline main(harness: Harness, _: unknown) {{
  harness.tools.clear_hooks()
  const tools = tool_define(
    tool_registry(),
    "slow_edit",
    "Writes, then keeps working until stopped.",
    {{parameters: {{}}, handler: {{ _args ->
      {write}
      harness.clock.sleep_ms(5000)
      return "should not arrive"
    }}}},
  )
  const counter = harness.runtime.shared_cell({{scope: "task_group", key: "{session}", initial: 0}})
  const mock_llm = {{ _call ->
    const snap = harness.runtime.shared_snapshot(counter)
    harness.runtime.shared_cas(counter, snap, snap.value + 1)
    if snap.value == 0 {{
      return {{ok: true, value: {{text: "", tool_calls: [{{id: "stopped_call", name: "slow_edit", arguments: {{}}}}], provider: "mock", model: "mock"}}}}
    }}
    return {{ok: true, value: {{text: "ok ##DONE##", tool_calls: [], provider: "mock", model: "mock"}}}}
  }}
  {canceller}
  const _ = try {{
    agent_loop(harness, "edit then wait", nil, {{
      provider: "mock", tools: tools, tool_format: "native", max_iterations: 4,
      loop_until_done: true, session_id: "{session}", llm_caller: mock_llm,
    }})
  }} catch {{
    nil
  }}
}}
"#
    );
    let sink = Arc::new(UpdateSink::default());
    let registered = sink.clone();
    let session_id = session.to_string();
    harn_vm::on_vm_stack(move || {
        // Reset first: it drops every sink registered before it.
        harn_vm::reset_thread_local_state();
        harn_vm::agent_events::register_sink(session_id, registered);
        let chunk = harn_vm::compile_source(&source).expect("source compiles");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
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
                    let _ = vm.execute(&chunk).await;
                    harn_vm::llm::clear_current_host_bridge();
                })
                .await;
        });
    });
    let updates = sink.updates.lock().unwrap();
    updates
        .iter()
        .rev()
        .find(|(id, status, ..)| {
            id == "stopped_call"
                && matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
        })
        .map(|(_, status, mutation, paths)| (*status, *mutation, paths.clone()))
        .unwrap_or_else(|| panic!("no terminal update for the stopped call: {updates:?}"))
}

fn assert_applied(mutation: ToolMutationStatus, paths: Option<Vec<String>>) {
    assert_eq!(mutation, ToolMutationStatus::Applied);
    let paths = paths.expect("an applied stop names the paths it changed");
    assert!(
        paths.iter().any(|path| path.ends_with("stopped-write.txt")),
        "{paths:?}"
    );
}

#[test]
fn an_in_flight_cancel_after_a_completed_write_reports_applied() {
    let (status, mutation, paths) = stopped_call_update("abandoned-mutation-inflight", true);
    assert_ne!(
        status,
        ToolCallStatus::Completed,
        "the call was stopped, not finished"
    );
    assert_applied(mutation, paths);
}

#[test]
fn a_stopped_call_that_wrote_nothing_stays_unknown() {
    let (status, mutation, paths) = stopped_call_update("abandoned-mutation-inflight-none", false);
    assert_ne!(
        status,
        ToolCallStatus::Completed,
        "the call was stopped, not finished"
    );
    assert_eq!(mutation, ToolMutationStatus::Unknown);
    assert_eq!(paths, None);
}
