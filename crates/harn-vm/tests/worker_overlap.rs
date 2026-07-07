#![recursion_limit = "256"]
//! Proof that Harn background agent workers run CONCURRENTLY.
//!
//! Each agent runs an `agent_loop` whose single LLM turn is stubbed via the
//! `llm_caller` seam. The stub brackets its `sleep(STALL_MS)` with two
//! host builtins — `__overlap_enter` / `__overlap_exit` — that maintain a
//! shared in-flight counter on the Rust side and record the high-water mark. N
//! background `sub_agent_run(...{background: true})` spawns are joined with
//! `wait_agent([...])`. If the workers overlap, all N stubs sit in their
//! `sleep` at once, so the in-flight counter reaches N.
//!
//! The assertion is therefore a *logical* peak-concurrency claim — the
//! recorded high-water mark equals `WORKER_COUNT` — instead of a brittle
//! wall-clock delta (`saved ≈ (N-1)*STALL_MS`) that compresses under CI load.
//! The `sleep` is retained purely as a virtual-time barrier that lets all N
//! workers park at the same time; the Tokio clock is paused and advanced in
//! process, so the test pays no wall-clock delay. The test also asserts every
//! agent completed its stubbed turn, so it cannot pass vacuously by skipping
//! work.
//!
//! The runtime is a paused current-thread tokio runtime driving a `LocalSet`;
//! Harn workers `spawn_local` onto that set, so the stub `sleep` resolves
//! against virtual Tokio time and concurrent sleeps genuinely overlap.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harn_vm::bridge::HostBridge;
use harn_vm::value::{VmError, VmValue};

/// Number of agents in each phase of the overlap proof.
const WORKER_COUNT: usize = 4;
/// Per-agent stub LLM stall, in milliseconds. Acts only as the barrier that
/// lets all background workers park in their stub simultaneously; no assertion
/// reads wall-clock time.
const STALL_MS: u64 = 200;

fn run_with_bridge(
    source: &str,
    register: impl FnOnce(&mut harn_vm::Vm) + 'static,
    peak: Arc<AtomicUsize>,
) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let vm_task = tokio::task::spawn_local(async move {
                    let bridge = Arc::new(HostBridge::from_parts(
                        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                        Arc::new(AtomicBool::new(false)),
                        Arc::new(Mutex::new(())),
                        1,
                    ));
                    harn_vm::llm::install_current_host_bridge(bridge.clone());
                    let mut vm = harn_vm::Vm::new();
                    harn_vm::register_vm_stdlib(&mut vm);
                    register(&mut vm);
                    let result = vm
                        .execute(&chunk)
                        .await
                        .map_err(|e: VmError| format!("{e:?}"));
                    let output = vm.output().to_string();
                    harn_vm::llm::clear_current_host_bridge();
                    result.map(|_| output)
                });

                for _ in 0..1_000 {
                    if peak.load(Ordering::SeqCst) >= WORKER_COUNT || vm_task.is_finished() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }

                if peak.load(Ordering::SeqCst) < WORKER_COUNT {
                    vm_task.abort();
                    return Err(format!(
                        "background workers never all parked before virtual-time advance; peak={}",
                        peak.load(Ordering::SeqCst)
                    ));
                }

                tokio::time::advance(Duration::from_millis(STALL_MS)).await;
                vm_task.await.map_err(|e| e.to_string())?
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

#[test]
fn background_workers_overlap_by_peak_in_flight_count() {
    // N background agents are joined with wait_agent, whose result envelope
    // nests each worker's own envelope under `result`. Each agent's stub LLM
    // turn brackets its sleep with __overlap_enter/__overlap_exit so the Rust
    // side observes the peak number of concurrently-parked stubs.
    let source = format!(
        r#"
fn make_caller() {{
  return {{ call ->
    __overlap_enter()
    sleep({STALL_MS})
    __overlap_exit()
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
  let handles = []
  for i in 0 to {WORKER_COUNT} exclusive {{
    handles = handles.push(sub_agent_run("bg worker " + to_string(i), base_opts() + {{background: true}}))
  }}
  const results = wait_agent(handles)

  let conc_done = 0
  for r in results {{
    const env = r?.result
    if (env?.ok ?? false) == true && contains(to_string(env?.summary ?? ""), "done") {{
      conc_done = conc_done + 1
    }}
  }}

  log("CONC_RESULTS=" + to_string(len(results)))
  log("CONC_DONE=" + to_string(conc_done))
}}
"#,
    );

    // Shared in-flight counter and high-water mark, mutated by the host
    // builtins the Harn stub calls around its sleep.
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let register = {
        let enter_in_flight = in_flight.clone();
        let enter_peak = peak.clone();
        let exit_in_flight = in_flight.clone();
        move |vm: &mut harn_vm::Vm| {
            vm.register_builtin("__overlap_enter", move |_args, _out| {
                let now = enter_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                enter_peak.fetch_max(now, Ordering::SeqCst);
                Ok(VmValue::Int(now as i64))
            });
            vm.register_builtin("__overlap_exit", move |_args, _out| {
                exit_in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(VmValue::Nil)
            });
        }
    };

    let raw = run_with_bridge(&source, register, peak.clone()).expect("overlap pipeline must run");
    let lines = out_lines(&raw);

    eprintln!("--- harn output ---");
    for line in &lines {
        eprintln!("{line}");
    }

    let conc_results = require_usize(&lines, "CONC_RESULTS");
    let conc_done = require_usize(&lines, "CONC_DONE");
    let peak_in_flight = peak.load(Ordering::SeqCst);
    let residual_in_flight = in_flight.load(Ordering::SeqCst);

    // (1) Non-vacuity: every background agent ran its stubbed LLM turn.
    assert_eq!(
        conc_results, WORKER_COUNT,
        "concurrent phase: wait_agent must return one result per worker; lines: {lines:?}"
    );
    assert_eq!(
        conc_done, WORKER_COUNT,
        "concurrent phase: every background worker must complete; lines: {lines:?}"
    );

    // (2) Bookkeeping sanity: every enter was matched by an exit.
    assert_eq!(
        residual_in_flight, 0,
        "every __overlap_enter must be matched by an __overlap_exit; residual={residual_in_flight}"
    );

    // (3) Overlap proof. If the background workers overlap, all N stubs sit in
    //     their `sleep` simultaneously, so the in-flight high-water mark equals
    //     WORKER_COUNT. If they ran serially the counter would never exceed 1.
    //     This is a structural concurrency claim with no wall-clock-delta math.
    assert_eq!(
        peak_in_flight, WORKER_COUNT,
        "OVERLAP REFUTED: peak concurrent in-flight workers was {peak_in_flight}, expected \
         {WORKER_COUNT}. Background workers did not all run concurrently. lines: {lines:?}"
    );

    eprintln!("OVERLAP CONFIRMED: N={WORKER_COUNT}, peak in-flight={peak_in_flight}");
}
