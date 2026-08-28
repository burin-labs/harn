//! Causal proof that no-middleware host-batch tool dispatch overlaps.
//!
//! `agent_dispatch_tool_batch` is the public host-batch seam. Each supplied
//! Harn handler parks on a Tokio `Barrier` (`__overlap_park`) that will not
//! return until `release_at` siblings have entered. Serial execution deadlocks
//! the park; a skipped handler never increments the shared counter.
//!
//! Named controls are serial (peak=1), capped (peak=2), and full overlap
//! (peak=N). A fourth case drives `agent_loop` with no `tool_caller` so the
//! production no-middleware path is held to the same shared-runtime claim.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harn_vm::bridge::HostBridge;
use harn_vm::value::{VmError, VmValue};

const WORKER_COUNT: usize = 4;
const OVERLAP_PROOF_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
enum OverlapProofFailure {
    Vm(String),
    TimedOut {
        release_at: usize,
        timeout: Duration,
    },
}

fn run_with_overlap_park(
    source: &str,
    release_at: usize,
    peak: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
) -> Result<String, OverlapProofFailure> {
    run_with_overlap_park_timeout(source, release_at, peak, in_flight, OVERLAP_PROOF_TIMEOUT)
}

fn run_with_overlap_park_timeout(
    source: &str,
    release_at: usize,
    peak: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    timeout: Duration,
) -> Result<String, OverlapProofFailure> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source).map_err(OverlapProofFailure::Vm)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| OverlapProofFailure::Vm(error.to_string()))?;
    let outcome = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        tokio::time::timeout(
            timeout,
            local.run_until(async {
                let bridge = Arc::new(HostBridge::from_parts(
                    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(())),
                    1,
                ));
                harn_vm::llm::install_current_host_bridge(bridge.clone());
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let enter_in_flight = in_flight.clone();
                let enter_peak = peak.clone();
                let barrier = Arc::new(tokio::sync::Barrier::new(release_at.max(1)));
                vm.register_async_builtin("__overlap_park", move |_ctx, _args| {
                    let in_flight = enter_in_flight.clone();
                    let peak = enter_peak.clone();
                    let barrier = barrier.clone();
                    async move {
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        barrier.wait().await;
                        Ok(VmValue::Int(now as i64))
                    }
                });
                let exit_in_flight = in_flight.clone();
                vm.register_builtin("__overlap_exit", move |_args, _out| {
                    exit_in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok(VmValue::Nil)
                });
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|error: VmError| OverlapProofFailure::Vm(format!("{error:?}")));
                let output = vm.output().to_string();
                result.map(|_| output)
            }),
        )
        .await
    });
    harn_vm::llm::clear_current_host_bridge();
    outcome.map_err(|_| OverlapProofFailure::TimedOut {
        release_at,
        timeout,
    })?
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

fn fanout_tools_source() -> &'static str {
    r#"
fn make_tools(release_at: int) {
  return tool_define(
    tool_registry(),
    "fanout_echo",
    "Park until siblings share one runtime in-flight count",
    {
      handler: { args ->
        __overlap_park(release_at)
        __overlap_exit()
        return "echoed:" + to_string(args?.msg ?? "")
      },
      parameters: {msg: {type: "string"}},
      annotations: {kind: "read", side_effect_level: "read_only"},
    },
  )
}

fn fanout_calls() {
  return [
    {id: "n1", name: "fanout_echo", arguments: {msg: "one"}},
    {id: "n2", name: "fanout_echo", arguments: {msg: "two"}},
    {id: "n3", name: "fanout_echo", arguments: {msg: "three"}},
    {id: "n4", name: "fanout_echo", arguments: {msg: "four"}},
  ]
}
"#
}

fn count_ok_results_source() -> &'static str {
    r"
fn count_ok(results: list) -> int {
  let ok = 0
  for r in results {
    if (r?.ok ?? false) == true {
      ok = ok + 1
    }
  }
  return ok
}
"
}

fn run_host_batch_case(label: &str, cap: usize) -> (Vec<String>, usize, usize) {
    let source = format!(
        r#"
import {{ agent_dispatch_tool_batch }} from "std/agent/primitives"
{tools}
{count}

pipeline main(harness: Harness, task: unknown) {{
  const results = agent_dispatch_tool_batch(
    harness.tools,
    fanout_calls(),
    make_tools({cap}),
    {{_max_concurrent_tools: {cap}}},
  )
  harness.stdio.log("{label}_RESULTS=" + to_string(len(results)))
  harness.stdio.log("{label}_OK=" + to_string(count_ok(results)))
}}
"#,
        tools = fanout_tools_source(),
        count = count_ok_results_source(),
    );

    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let raw = run_with_overlap_park(&source, cap, peak.clone(), in_flight.clone())
        .unwrap_or_else(|error| panic!("{label} host-batch pipeline must run: {error:?}"));
    let lines = out_lines(&raw);
    assert_eq!(
        require_usize(&lines, &format!("{label}_RESULTS")),
        WORKER_COUNT,
        "{label}: host-batch must return one result per call; {lines:?}"
    );
    assert_eq!(
        require_usize(&lines, &format!("{label}_OK")),
        WORKER_COUNT,
        "{label}: every host-batch handler must complete; {lines:?}"
    );
    assert_eq!(
        in_flight.load(Ordering::SeqCst),
        0,
        "{label}: every __overlap_park must be matched by __overlap_exit"
    );
    (lines, peak.load(Ordering::SeqCst), cap)
}

#[test]
fn overlap_proof_times_out_when_the_causal_barrier_cannot_release() {
    let timeout = Duration::from_millis(25);
    let failure = run_with_overlap_park_timeout(
        "pipeline main(harness: Harness, task: unknown) { __overlap_park(2) }",
        2,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        timeout,
    )
    .expect_err("an unreleased causal barrier must fail within its own bound");

    assert_eq!(
        failure,
        OverlapProofFailure::TimedOut {
            release_at: 2,
            timeout,
        }
    );
}

#[test]
fn host_batch_handlers_share_runtime_and_overlap() {
    let (lines, peak, cap) = run_host_batch_case("HOST_BATCH", WORKER_COUNT);
    assert_eq!(
        peak, cap,
        "OVERLAP REFUTED: host-batch peak={peak}, expected {cap}. \
         Handlers did not share one runtime in-flight count. {lines:?}"
    );
}

#[test]
fn host_batch_serial_cap_never_overlaps() {
    let (lines, peak, cap) = run_host_batch_case("HOST_BATCH_SERIAL", 1);
    assert_eq!(
        peak, cap,
        "serial host-batch must keep peak in-flight at 1; {lines:?}"
    );
}

#[test]
fn host_batch_capped_overlap_is_exactly_the_cap() {
    let (lines, peak, cap) = run_host_batch_case("HOST_BATCH_CAPPED", 2);
    assert_eq!(peak, cap, "capped host-batch must peak at {cap}; {lines:?}");
}

#[test]
fn agent_loop_no_middleware_handlers_share_runtime_and_overlap() {
    let source = format!(
        r###"
import {{ agent_loop }} from "std/agent/loop"
{tools}

pipeline main(harness: Harness, task: unknown) {{
  const cap = {WORKER_COUNT}
  const turn = harness.runtime.atomic(0)
  const routed = {{ _call ->
    const n = harness.runtime.atomic_add(turn, 1) + 1
    if n == 1 {{
      return {{
        ok: true,
        value: {{
          tool_calls: fanout_calls(),
          provider: "mock",
          model: "overlap-model",
        }},
      }}
    }}
    return {{
      ok: true,
      value: {{text: "##DONE##", provider: "mock", model: "overlap-model"}},
    }}
  }}
  const result = agent_loop(
    harness,
    "fan out without middleware",
    nil,
    {{
      provider: "mock",
      tools: make_tools(cap),
      tool_format: "native",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "##DONE##",
      max_concurrent_tools: cap,
      llm_caller: routed,
    }},
  )
  harness.stdio.log("LOOP_NO_MW_STATUS=" + result.status)
}}
"###,
        tools = fanout_tools_source(),
    );

    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let raw = run_with_overlap_park(&source, WORKER_COUNT, peak.clone(), in_flight.clone())
        .expect("agent_loop no-middleware overlap pipeline must run");
    let lines = out_lines(&raw);
    assert_eq!(
        parse_kv(&lines, "LOOP_NO_MW_STATUS"),
        Some("done"),
        "agent_loop no-middleware must finish; {lines:?}"
    );
    assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(
        peak.load(Ordering::SeqCst),
        WORKER_COUNT,
        "OVERLAP REFUTED: agent_loop no-middleware peak={}, expected {}. \
         Handlers did not share one runtime in-flight count. {lines:?}",
        peak.load(Ordering::SeqCst),
        WORKER_COUNT
    );
}
