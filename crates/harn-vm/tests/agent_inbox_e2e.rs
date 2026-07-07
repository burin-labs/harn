#![recursion_limit = "256"]
//! End-to-end coverage for the unified agent inbox.
//!
//! These tests exercise the public host-builtin surface
//! (`agent_session_post_event`, `agent_session_drain_feedback`,
//! `agent_session_inject_feedback`) plus the Rust-side
//! `crate::orchestration::agent_inbox` API to verify:
//!
//!   * Entries pushed from outside the loop preserve FIFO ordering.
//!   * `agent_session_drain_feedback` returns entries with the
//!     structured `{kind, content, source, sequence, ts_ms}` shape.
//!   * Pushes that happen during an awaited async future (simulating
//!     a long-running compaction LLM call) are still drainable
//!     afterwards — the inbox must not lose them mid-flight.

use harn_vm::value::VmError;

fn run(source: &str) -> Result<String, String> {
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
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out(source: &str) -> Vec<String> {
    let raw = run(source).expect("script failed");
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn post_event_then_drain_round_trips_through_inbox() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  agent_session_post_event(s, "tool_result", "first", "test")
  agent_session_post_event(s, "mcp_progress", "halfway", "mcp")
  agent_session_post_event(s, "tool_result", "second", "test")
  const drained = agent_session_drain_inbox(s)
  log(len(drained))
  log(drained[0].kind)
  log(drained[0].content)
  log(drained[0].source)
  log(drained[1].kind)
  log(drained[1].content)
  log(drained[2].kind)
  log(drained[2].content)
  log(drained[0].sequence < drained[1].sequence)
  log(drained[1].sequence < drained[2].sequence)
}
"#);
    assert_eq!(
        lines,
        vec![
            "3",
            "tool_result",
            "first",
            "test",
            "mcp_progress",
            "halfway",
            "tool_result",
            "second",
            "true",
            "true",
        ]
    );
}

#[test]
fn inbox_survives_concurrent_pushes_during_awaited_future() {
    // Race coverage at the Rust level — proves that the inbox does not
    // drop entries when a producer pushes while a consumer is parked
    // on an unrelated await. This is the structural guarantee that the
    // agent loop relies on across a long compaction LLM call.
    let session_id = format!("inbox-race-{}", uuid::Uuid::now_v7());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Simulate "compaction is running" by parking on a
                // tokio::sync::Notify the producer wakes us with —
                // logically equivalent to a long LLM call, but with
                // no wall-clock dependency. Tests must not poll
                // Instant::now or sleep wall-clock time.
                let signal = std::sync::Arc::new(tokio::sync::Notify::new());
                let producer_signal = signal.clone();
                let sid_a = session_id.clone();
                let producer = tokio::task::spawn_local(async move {
                    tokio::task::yield_now().await;
                    harn_vm::orchestration::agent_inbox::push(
                        &sid_a,
                        "tool_result",
                        "during",
                        "test.compaction_race",
                    );
                    harn_vm::orchestration::agent_inbox::push(
                        &sid_a,
                        "mcp_progress",
                        "also during",
                        "test.compaction_race",
                    );
                    producer_signal.notify_one();
                });
                // The "compaction" awaits a future the producer
                // completes; this is the structural equivalent of an
                // LLM call awaiting its provider. Production code
                // would call `vm_call_llm_full` here.
                signal.notified().await;
                producer.await.unwrap();
                // After the awaited future returns, drain — both
                // entries must still be there, in push order.
                let drained = harn_vm::orchestration::agent_inbox::drain(&session_id);
                assert_eq!(drained.len(), 2);
                assert_eq!(drained[0].content, "during");
                assert_eq!(drained[1].content, "also during");
                assert!(drained[0].sequence < drained[1].sequence);
            })
            .await;
    });
}

#[test]
fn drain_resets_inbox_between_calls() {
    // Two consecutive drains: the second should see only entries
    // pushed AFTER the first drain — proves the loop's
    // drain-before-compact / drain-after-compact pattern doesn't
    // double-deliver.
    let session_id = format!("inbox-drain-{}", uuid::Uuid::now_v7());
    harn_vm::orchestration::agent_inbox::push(&session_id, "k1", "a", "test");
    harn_vm::orchestration::agent_inbox::push(&session_id, "k2", "b", "test");
    let first = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(first.len(), 2);
    harn_vm::orchestration::agent_inbox::push(&session_id, "k3", "c", "test");
    let second = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].content, "c");
}
