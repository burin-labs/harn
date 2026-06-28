#![recursion_limit = "256"]
//! Proof that Harn background agent workers run CONCURRENTLY.
//!
//! Each agent runs an `agent_loop` whose single LLM turn is stubbed via the
//! `llm_caller` seam to `sleep(STALL_MS)` before returning a clean `done`
//! response. The test runs the *same* N-agent workload twice:
//!
//!   1. **Serial baseline** — N foreground `sub_agent_run` calls, one at a
//!      time. Total ≈ `N * (per_agent_overhead + STALL_MS)`.
//!   2. **Concurrent** — N background `sub_agent_run(...{background: true})`
//!      spawns joined with `wait_agent([...])`. If the workers overlap, the N
//!      stub `sleep`s collapse onto each other, so this run is shorter by
//!      ≈ `(N-1) * STALL_MS`.
//!
//! Because both phases pay the *identical* per-agent synchronous overhead
//! (session init, prompt assembly, compaction checkpoints), the *difference*
//! between them isolates exactly the overlapped sleep time — a far more honest
//! signal than a fixed wall-clock threshold. If the background workers ran
//! serially, the two phases would take the same time and `saved ≈ 0`.
//!
//! The runtime is a real (non-paused) current-thread tokio runtime driving a
//! `LocalSet`; Harn workers `spawn_local` onto that set, so the stub `sleep`
//! resolves against the real `tokio::time::sleep` and concurrent sleeps
//! genuinely overlap in wall-clock time. The test also asserts every agent in
//! both phases actually completed its stubbed turn, so it cannot pass
//! vacuously by skipping work.

use std::time::Instant;

use harn_vm::bridge::HostBridge;
use harn_vm::value::VmError;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Number of agents in each phase of the overlap proof.
const WORKER_COUNT: usize = 4;
/// Per-agent stub LLM stall, in milliseconds.
const STALL_MS: u64 = 200;

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

fn parse_kv<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

fn require_usize(lines: &[String], key: &str) -> usize {
    parse_kv(lines, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("missing/unparseable {key}; lines: {lines:?}"))
}

fn require_u64(lines: &[String], key: &str) -> u64 {
    parse_kv(lines, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("missing/unparseable {key}; lines: {lines:?}"))
}

#[test]
fn background_workers_overlap_in_wall_clock_time() {
    // Phase 1 runs N agents serially (foreground sub_agent_run, where the
    // result envelope is returned directly). Phase 2 runs the same N agents
    // as background workers joined with wait_agent (where each wait result
    // nests the envelope under `result`). Each agent's stub LLM turn sleeps
    // STALL_MS then returns a clean text completion.
    let source = format!(
        r#"
fn make_caller() {{
  return {{ call ->
    sleep({STALL_MS})
    return {{
      ok: true,
      value: {{
        text: "<user_response>done</user_response>\n<done>##DONE##</done>",
        provider: "mock",
        model: "overlap-model",
        input_tokens: 0,
        output_tokens: 0,
      }},
    }}
  }}
}}

fn base_opts() {{
  return {{
    provider: "mock",
    model: "overlap-model",
    llm_caller: make_caller(),
    loop_until_done: true,
    done_judge: false,
    max_iterations: 1,
    final_wrapup: false,
    tool_format: "text",
  }}
}}

pipeline main(task) {{
  // ---- Phase 1: serial baseline (foreground, one agent at a time) ----
  let serial_start = monotonic_ms()
  var serial_done = 0
  for i in 0 to {WORKER_COUNT} exclusive {{
    let res = sub_agent_run("serial worker " + to_string(i), base_opts() + {{background: false}})
    if (res?.ok ?? false) == true && contains(to_string(res?.summary ?? ""), "done") {{
      serial_done = serial_done + 1
    }}
  }}
  let serial_ms = monotonic_ms() - serial_start

  // ---- Phase 2: concurrent (background workers + wait_agent) ----
  var handles = []
  for i in 0 to {WORKER_COUNT} exclusive {{
    handles = handles.push(sub_agent_run("bg worker " + to_string(i), base_opts() + {{background: true}}))
  }}
  let conc_start = monotonic_ms()
  let results = wait_agent(handles)
  let conc_ms = monotonic_ms() - conc_start

  var conc_done = 0
  for r in results {{
    let env = r?.result
    if (env?.ok ?? false) == true && contains(to_string(env?.summary ?? ""), "done") {{
      conc_done = conc_done + 1
    }}
  }}

  log("SERIAL_DONE=" + to_string(serial_done))
  log("CONC_RESULTS=" + to_string(len(results)))
  log("CONC_DONE=" + to_string(conc_done))
  log("SERIAL_MS=" + to_string(serial_ms))
  log("CONC_MS=" + to_string(conc_ms))
}}
"#,
    );

    let wall = Instant::now();
    let raw = run_with_bridge(&source).expect("overlap pipeline must run");
    let total_wall = wall.elapsed();
    let lines = out_lines(&raw);

    eprintln!("--- harn output ---");
    for line in &lines {
        eprintln!("{line}");
    }
    eprintln!("--- total host wall: {total_wall:?} ---");

    let serial_done = require_usize(&lines, "SERIAL_DONE");
    let conc_results = require_usize(&lines, "CONC_RESULTS");
    let conc_done = require_usize(&lines, "CONC_DONE");
    let serial_ms = require_u64(&lines, "SERIAL_MS");
    let conc_ms = require_u64(&lines, "CONC_MS");

    // (1) Non-vacuity: every agent in BOTH phases ran its stubbed LLM turn.
    assert_eq!(
        serial_done, WORKER_COUNT,
        "serial baseline: every agent must complete; lines: {lines:?}"
    );
    assert_eq!(
        conc_results, WORKER_COUNT,
        "concurrent phase: wait_agent must return one result per worker; lines: {lines:?}"
    );
    assert_eq!(
        conc_done, WORKER_COUNT,
        "concurrent phase: every background worker must complete; lines: {lines:?}"
    );

    // (2) Sanity floor: the stub really slept. The concurrent phase still
    //     spends at least one full STALL_MS overlapping the sleeps.
    assert!(
        conc_ms >= STALL_MS / 2,
        "concurrent window {conc_ms}ms is implausibly small — did the stub sleep run? \
         (STALL_MS={STALL_MS}); lines: {lines:?}"
    );

    // (3) Overlap proof. If the workers overlap, the concurrent phase collapses
    //     N stub sleeps onto each other, saving ≈ (N-1) * STALL_MS versus the
    //     serial baseline (which pays the identical per-agent overhead). We
    //     require realizing at least 60% of that theoretical saving — well
    //     above zero (serial workers) yet tolerant of scheduling jitter.
    let saved_ms = serial_ms.saturating_sub(conc_ms);
    let theoretical_overlap_ms = (WORKER_COUNT as u64 - 1) * STALL_MS;
    let required_saving_ms = theoretical_overlap_ms * 6 / 10;

    assert!(
        saved_ms >= required_saving_ms,
        "OVERLAP REFUTED: serial={serial_ms}ms concurrent={conc_ms}ms saved={saved_ms}ms \
         — expected to save >= {required_saving_ms}ms (60% of {theoretical_overlap_ms}ms of \
         overlappable sleep across {WORKER_COUNT} agents). Background workers did not run \
         concurrently. lines: {lines:?}"
    );

    eprintln!(
        "OVERLAP CONFIRMED: N={WORKER_COUNT}, STALL={STALL_MS}ms, serial≈{serial_ms}ms, \
         concurrent≈{conc_ms}ms, saved≈{saved_ms}ms (>= required {required_saving_ms}ms; \
         theoretical overlap {theoretical_overlap_ms}ms)"
    );
}
