#![recursion_limit = "256"]
//! PausedClock integration proof for `command_ledger_hold` — the orchestration
//! that PARKS the whole agent loop while a long-running command is awaited.
//!
//! The pure decision functions (`command_ledger_decide`, digest building,
//! scheduling) are unit-tested in `tests/agent/command_ledger_test.harn`, and
//! the raw park primitive `agent_inbox::wait_async` has its own three-outcome
//! `PausedClock` proof. This file closes the gap those cannot: it runs the REAL
//! `command_ledger_hold` harn function through a fully-configured stdlib VM,
//! with a `Harness::test` `PausedClock` installed as the `harness` global so the
//! builtin park and the loop's own deadline math read the SAME virtual clock,
//! and drives virtual time + inbox pushes from the test thread. Never
//! `mock_time`: `MockAwareClock::sleep` returns instantly under a mock and would
//! make the park time out immediately, silently defeating the hold.
//!
//! It exercises the five behaviors that only manifest on a live VM (and that the
//! silent builtin-registration no-op bug proved unit gates cannot cover):
//!   (a) park -> wake -> drain -> digest ordering;
//!   (b) the delta gate suppresses a no-change scheduled re-entry;
//!   (c) the awaited-wall ceiling auto-resolves per surface (kill in headless,
//!       release-to-service in interactive);
//!   (d) re-entry budget exhaustion returns the `await_exhausted` recovery
//!       outcome instead of killing;
//!   (e) the wake set is INCLUSIVE — a non-command inbox entry (user feedback)
//!       wakes the park and SURVIVES the drain-by-kind for the normal path.
//!
//! Each test uses a distinct session id so the process-global inbox never
//! collides under cargo's parallel test threads.

use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harn_clock::PausedClock;
use harn_vm::bridge::HostBridge;
use harn_vm::harness::Harness;
use harn_vm::orchestration::agent_inbox;
use harn_vm::value::VmError;

const TERMINAL_JSON: &str = r#"{"handle_id":"hto-1","status":"completed","exit_code":0,"duration_ms":90000,"stdout":"done","output_path":"/tmp/o.log"}"#;

/// A harn snippet that ingests one awaited running handle and calls
/// `command_ledger_hold` once, logging the outcome, whether a digest came back,
/// and the surviving ledger length. `reentries` seeds the hold-state budget;
/// `wall_ms` overrides the awaited-wall ceiling.
fn hold_snippet(session: &str, reentries: i64, wall_ms: i64) -> String {
    format!(
        r#"
import {{
  command_wait_normalize,
  command_ledger_new,
  command_ledger_ingest,
  command_ledger_hold,
}} from "std/agent/command_ledger"

pipeline main() {{
  const cw = command_wait_normalize({{max_awaited_wall_ms: {wall_ms}}})
  const running = {{
    status: "running",
    handle_id: "hto-1",
    command_or_op_descriptor: "cargo build --release",
    output_offset: 100,
    byte_count: 100,
    stderr_byte_count: 0,
    silence_ms: 0,
    stdout: "compiling",
  }}
  const ledger = command_ledger_ingest(command_ledger_new(), [running], 0, cw)
  const r = command_ledger_hold("{session}", ledger, cw, {{reentries: {reentries}}})
  log("OUTCOME=" + r.outcome)
  log("HAS_DIGEST=" + to_string(r?.digest != nil))
  log("LEDGER_LEN=" + to_string(len(r.ledger)))
  if r?.digest != nil {{
    log("DIGEST=" + r.digest)
  }}
}}
"#
    )
}

/// A harn snippet that drives the ceiling twice: the first hold breaches the
/// tiny wall and ESCALATES (returns a digest, row marked escalated); the second
/// hold, still past the ceiling with the escalated ledger, AUTO-RESOLVES per the
/// configured surface. Logs the second outcome + the surviving ledger shape.
fn ceiling_snippet(session: &str, auto_resolve: &str) -> String {
    format!(
        r#"
import {{
  command_wait_normalize,
  command_ledger_new,
  command_ledger_ingest,
  command_ledger_hold,
}} from "std/agent/command_ledger"

fn _lease_of(ledger) {{
  for row in ledger {{
    return to_string(row?.lease ?? "")
  }}
  return "none"
}}

pipeline main() {{
  const cw = command_wait_normalize({{max_awaited_wall_ms: 100, auto_resolve: "{auto_resolve}"}})
  const running = {{status: "running", handle_id: "hto-1", command_or_op_descriptor: "svc", output_offset: 0, byte_count: 0, stderr_byte_count: 0, silence_ms: 0, stdout: ""}}
  const ledger = command_ledger_ingest(command_ledger_new(), [running], 0, cw)
  const r1 = command_ledger_hold("{session}", ledger, cw, {{reentries: 0}})
  log("R1_OUTCOME=" + r1.outcome)
  const r2 = command_ledger_hold("{session}", r1.ledger, cw, r1.hold_state)
  log("R2_OUTCOME=" + r2.outcome)
  log("R2_LEN=" + to_string(len(r2.ledger)))
  log("R2_LEASE=" + _lease_of(r2.ledger))
}}
"#
    )
}

fn out_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

fn line_value<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

fn make_vm() -> (harn_vm::Vm, Arc<PausedClock>) {
    let (harness, paused) = Harness::test();
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    vm.set_global("harness", harness.into_vm_value());
    (vm, paused)
}

fn install_bridge() {
    let bridge = Arc::new(HostBridge::from_parts(
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(())),
        1,
    ));
    harn_vm::llm::install_current_host_bridge(bridge);
}

type HoldTask = tokio::task::JoinHandle<Result<String, String>>;

/// Compile + run a hold snippet on a `LocalSet`, handing the paused clock and
/// the still-spawning VM task to an async `driver` that choreographs the park
/// (yield to park, advance virtual time, push inbox entries, assert park state)
/// and awaits the task's raw output. Returns the parsed `[harn] key=value` lines.
fn run_scenario<D, F>(snippet: String, driver: D) -> Vec<String>
where
    D: FnOnce(Arc<PausedClock>, HoldTask) -> F,
    F: Future<Output = String>,
{
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(&snippet).expect("snippet compiles");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (mut vm, paused) = make_vm();
                let task: HoldTask = tokio::task::spawn_local(async move {
                    install_bridge();
                    let result = vm
                        .execute(&chunk)
                        .await
                        .map_err(|e: VmError| format!("{e:?}"));
                    let output = vm.output().to_string();
                    harn_vm::llm::clear_current_host_bridge();
                    result.map(|_| output)
                });
                let raw = driver(paused, task).await;
                out_lines(&raw)
            })
            .await
    })
}

async fn yield_times(n: usize) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}

// (a): a terminal wakes the park -> the loop drains it -> re-enters with a
// digest. The `is_finished()` gate proves it actually PARKED (did not resolve
// synchronously) before the push woke it.
#[test]
fn park_wakes_on_terminal_then_returns_digest() {
    let session = "hold-terminal-sess";
    let lines = run_scenario(
        hold_snippet(session, 0, 900_000),
        |_paused, task| async move {
            yield_times(8).await;
            assert!(
                !task.is_finished(),
                "hold must PARK while the awaited handle runs, not resolve synchronously"
            );
            agent_inbox::push(session, "tool_result", TERMINAL_JSON, "test");
            task.await.expect("join").expect("snippet ok")
        },
    );
    assert_eq!(
        line_value(&lines, "OUTCOME"),
        Some("digest"),
        "lines: {lines:?}"
    );
    assert_eq!(
        line_value(&lines, "HAS_DIGEST"),
        Some("true"),
        "lines: {lines:?}"
    );
    let digest = line_value(&lines, "DIGEST").unwrap_or("");
    assert!(
        digest.contains("command_status") && digest.contains("completed"),
        "digest must carry the completed handle: {digest}"
    );
}

// (e): the wake set is INCLUSIVE. A non-command inbox entry (user feedback)
// wakes the park; the ledger's drain-by-kind leaves it untouched, so the hold
// returns `interrupted` (hand back to the normal loop) and the user entry
// SURVIVES for the normal feedback path.
#[test]
fn non_command_entry_wakes_park_and_survives_drain() {
    let session = "hold-inclusive-sess";
    let lines = run_scenario(
        hold_snippet(session, 0, 900_000),
        |_paused, task| async move {
            yield_times(8).await;
            assert!(!task.is_finished(), "hold must park before the interrupt");
            agent_inbox::push(session, "user_interrupt", "stop please", "user");
            task.await.expect("join").expect("snippet ok")
        },
    );
    assert_eq!(
        line_value(&lines, "OUTCOME"),
        Some("interrupted"),
        "a non-command wake must hand back to the loop, not fabricate a digest: {lines:?}"
    );
    // The drain-by-kind must NOT have consumed the user entry.
    let remaining = agent_inbox::drain(session);
    assert_eq!(
        remaining.len(),
        1,
        "user feedback must survive the drain: {remaining:?}"
    );
    assert_eq!(remaining[0].kind, "user_interrupt");
}

// (b): the delta gate suppresses a no-change scheduled re-entry. Advancing to
// the 30s decision rung with NO new output must NOT fire a digest — the hold
// re-parks (schedule doubled). Only the later terminal ends it.
#[test]
fn delta_gate_suppresses_no_change_rung() {
    let session = "hold-deltagate-sess";
    let lines = run_scenario(
        hold_snippet(session, 0, 900_000),
        |paused, task| async move {
            yield_times(8).await;
            assert!(!task.is_finished(), "hold must park at t=0");
            // Reach the first 30s rung with zero new output, then let the woken loop
            // re-decide and re-park.
            paused.advance(Duration::from_secs(30));
            yield_times(8).await;
            assert!(
                !task.is_finished(),
                "a no-output decision rung must be delta-gated (re-park), not fire a digest"
            );
            // A real terminal now ends the hold.
            agent_inbox::push(session, "tool_result", TERMINAL_JSON, "test");
            task.await.expect("join").expect("snippet ok")
        },
    );
    assert_eq!(
        line_value(&lines, "OUTCOME"),
        Some("digest"),
        "lines: {lines:?}"
    );
    let digest = line_value(&lines, "DIGEST").unwrap_or("");
    assert!(
        digest.contains("completed"),
        "final digest is the terminal: {digest}"
    );
}

// (d): re-entry budget exhaustion returns `exhausted` (the loop maps this to the
// `await_exhausted` stuck-detector recovery) instead of parking or killing.
#[test]
fn budget_exhaustion_returns_await_exhausted_outcome() {
    let session = "hold-budget-sess";
    // reentries == default budget (8): the hold returns before parking.
    let lines = run_scenario(
        hold_snippet(session, 8, 900_000),
        |_paused, task| async move { task.await.expect("join").expect("snippet ok") },
    );
    assert_eq!(
        line_value(&lines, "OUTCOME"),
        Some("exhausted"),
        "a spent re-entry budget must recover, not park or kill: {lines:?}"
    );
}

// (c) headless: the awaited-wall ceiling auto-resolves by KILLING — the handle
// is cancelled and the row retired.
#[test]
fn ceiling_auto_resolve_kills_in_headless_surface() {
    let session = "hold-ceiling-kill-sess";
    let lines = run_scenario(
        ceiling_snippet(session, "kill"),
        |paused, task| async move {
            yield_times(8).await;
            assert!(
                !task.is_finished(),
                "first hold must park until the ceiling deadline"
            );
            paused.advance(Duration::from_millis(100));
            task.await.expect("join").expect("snippet ok")
        },
    );
    assert_eq!(
        line_value(&lines, "R1_OUTCOME"),
        Some("digest"),
        "escalation first: {lines:?}"
    );
    assert_eq!(
        line_value(&lines, "R2_OUTCOME"),
        Some("resolved"),
        "then auto-resolve: {lines:?}"
    );
    assert_eq!(
        line_value(&lines, "R2_LEN"),
        Some("0"),
        "kill retires the row: {lines:?}"
    );
}

// (c) interactive: the awaited-wall ceiling auto-resolves by RELEASING to a
// service lease — the process keeps running, off the awaited schedule.
#[test]
fn ceiling_auto_resolve_releases_in_interactive_surface() {
    let session = "hold-ceiling-release-sess";
    let lines = run_scenario(
        ceiling_snippet(session, "release"),
        |paused, task| async move {
            yield_times(8).await;
            assert!(
                !task.is_finished(),
                "first hold must park until the ceiling deadline"
            );
            paused.advance(Duration::from_millis(100));
            task.await.expect("join").expect("snippet ok")
        },
    );
    assert_eq!(
        line_value(&lines, "R1_OUTCOME"),
        Some("digest"),
        "escalation first: {lines:?}"
    );
    assert_eq!(
        line_value(&lines, "R2_OUTCOME"),
        Some("resolved"),
        "then auto-resolve: {lines:?}"
    );
    assert_eq!(
        line_value(&lines, "R2_LEN"),
        Some("1"),
        "release keeps the row: {lines:?}"
    );
    assert_eq!(
        line_value(&lines, "R2_LEASE"),
        Some("service"),
        "as a service lease: {lines:?}"
    );
}
