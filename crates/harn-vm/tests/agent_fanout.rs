#![recursion_limit = "256"]
//! Contract lock for the `agent_fanout(requests, options)` stdlib primitive
//! (crates/harn-stdlib/src/stdlib/agent/workers.harn).
//!
//! `agent_fanout` maps a list of independent units onto concurrent background
//! sub-agents, runs them in bounded waves of `max_parallel`, joins each wave,
//! and returns one normalized `{label, index, status, ok, result, error}` per
//! request in INPUT order. These tests lock the *contract* (order, labels,
//! per-child isolation, ok-normalization, wave chunking) — not timing.
//!
//! Each child's single LLM turn is stubbed via the `llm_caller` seam: a clean
//! child returns `<user_response>MARKER</user_response>` (which becomes the
//! sub-agent envelope's `summary`), so a result whose `result.summary` equals
//! the child's OWN marker proves no cross-talk between siblings. A failing
//! child's stub returns `{ok: false, ...}`, which (with no consecutive-failure
//! budget configured) terminates that child's loop immediately without
//! disturbing its siblings.
//!
//! The runtime mirrors worker_overlap.rs: a real current-thread tokio runtime
//! driving a `LocalSet`, with an installed `HostBridge`, so Harn workers
//! `spawn_local` onto that set.

use harn_vm::bridge::HostBridge;
use harn_vm::value::VmError;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

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

/// A parsed `R=` row emitted by the test pipelines, one per fanout result.
#[derive(Debug, Clone)]
struct Row {
    index: usize,
    label: String,
    status: String,
    ok: bool,
    summary: String,
    error_present: bool,
}

/// Rows are logged as `R=<index>|<label>|<status>|<ok>|<summary>|<error_present>`.
/// Markers, labels, and statuses contain no `|`, so a fixed split is safe.
fn parse_rows(lines: &[String]) -> Vec<Row> {
    let mut rows: Vec<Row> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("R="))
        .map(|rest| {
            let parts: Vec<&str> = rest.split('|').collect();
            assert!(
                parts.len() == 6,
                "malformed R= row (expected 6 fields): {rest:?}"
            );
            Row {
                index: parts[0].parse().expect("index"),
                label: parts[1].to_string(),
                status: parts[2].to_string(),
                ok: parts[3] == "true",
                summary: parts[4].to_string(),
                error_present: parts[5] == "true",
            }
        })
        .collect();
    // Defend against the rows arriving out of emission order: the contract is
    // about the `index` field, so sort by it before asserting input order.
    rows.sort_by_key(|r| r.index);
    rows
}

/// Shared Harn prelude: a per-marker clean stub caller and the base child
/// options. `ok_caller(marker)` captures `marker` so each child returns its
/// OWN response — the basis of the no-cross-talk check.
const PRELUDE: &str = r#"
fn ok_caller(marker) {
  return { call ->
    sleep(15)
    return {
      ok: true,
      value: {
        text: "<user_response>" + marker + "</user_response>\n<done>##DONE##</done>",
        provider: "mock",
        model: "fanout-model",
        input_tokens: 0,
        output_tokens: 0,
      },
    }
  }
}

fn fail_caller(marker) {
  return { call ->
    sleep(15)
    // Throw so the child's agent_loop unwinds and execute_sub_agent wraps it
    // as an `ok: false` sub-agent envelope with a non-nil error. (A graceful
    // `{ok: false}` return is instead absorbed into a "completed" envelope.)
    throw "stubbed failure for " + marker
  }
}

fn base_opts(caller) {
  return {
    provider: "mock",
    model: "fanout-model",
    llm_caller: caller,
    loop_until_done: true,
    done_judge: false,
    max_iterations: 1,
    final_wrapup: false,
    tool_format: "text",
  }
}

fn emit_rows(results) {
  for r in results {
    let summary = to_string(r?.result?.summary ?? "")
    let err_present = to_string(r?.error != nil)
    log(
      "R=" + to_string(r?.index) + "|" + to_string(r?.label) + "|"
        + to_string(r?.status) + "|" + to_string(r?.ok) + "|" + summary + "|" + err_present,
    )
  }
}
"#;

#[test]
fn fanout_preserves_order_labels_and_isolates_children() {
    // Six requests, distinct labels + distinct markers, default max_parallel.
    let source = format!(
        r#"{PRELUDE}
pipeline main(task) {{
  let labels = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
  var reqs = []
  var i = 0
  for label in labels {{
    let marker = "MARK-" + to_string(i)
    reqs = reqs.push({{task: "do " + marker, options: base_opts(ok_caller(marker)), label: label}})
    i = i + 1
  }}
  let results = agent_fanout(reqs, {{max_parallel: 8}})
  log("COUNT=" + to_string(len(results)))
  emit_rows(results)
}}
"#
    );

    let raw = run_with_bridge(&source).expect("fanout pipeline must run");
    let lines = out_lines(&raw);
    eprintln!("--- harn output ---\n{}", lines.join("\n"));

    let rows = parse_rows(&lines);
    let labels = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    assert_eq!(rows.len(), labels.len(), "lines: {lines:?}");

    for (i, expected_label) in labels.iter().enumerate() {
        let row = &rows[i];
        // ORDER + index: result i has index i.
        assert_eq!(
            row.index, i,
            "result {i} carries wrong index; lines: {lines:?}"
        );
        // LABEL: result i carries request i's label.
        assert_eq!(
            &row.label, expected_label,
            "result {i} label mismatch; lines: {lines:?}"
        );
        // NO CROSS-TALK: child i's own envelope summary == child i's marker.
        assert_eq!(
            row.summary,
            format!("MARK-{i}"),
            "result {i} (label {expected_label}) returned a sibling's marker — cross-talk! \
             lines: {lines:?}"
        );
        // OK normalization on a clean child.
        assert!(row.ok, "result {i} should be ok; lines: {lines:?}");
        assert!(
            !row.error_present,
            "result {i} should have no error; lines: {lines:?}"
        );
    }
    eprintln!("ORDER+LABELS+NO-CROSSTALK CONFIRMED: 6 children, each returned its own marker in input order");
}

#[test]
fn fanout_isolates_a_single_child_failure() {
    // Five children; index 2 ("charlie") fails cleanly, the rest succeed.
    let source = format!(
        r#"{PRELUDE}
pipeline main(task) {{
  let labels = ["alpha", "bravo", "charlie", "delta", "echo"]
  var reqs = []
  var i = 0
  for label in labels {{
    let marker = "MARK-" + to_string(i)
    let caller = if i == 2 {{ fail_caller(marker) }} else {{ ok_caller(marker) }}
    reqs = reqs.push({{task: "do " + marker, options: base_opts(caller), label: label}})
    i = i + 1
  }}
  let results = agent_fanout(reqs, {{max_parallel: 8}})
  log("COUNT=" + to_string(len(results)))
  emit_rows(results)
}}
"#
    );

    let raw = run_with_bridge(&source).expect("fanout pipeline must run");
    let lines = out_lines(&raw);
    eprintln!("--- harn output ---\n{}", lines.join("\n"));

    let rows = parse_rows(&lines);
    let labels = ["alpha", "bravo", "charlie", "delta", "echo"];
    assert_eq!(rows.len(), labels.len(), "lines: {lines:?}");

    for (i, expected_label) in labels.iter().enumerate() {
        let row = &rows[i];
        assert_eq!(row.index, i, "lines: {lines:?}");
        assert_eq!(&row.label, expected_label, "lines: {lines:?}");
        // The worker lifecycle completes for every child — even the failed
        // agent (whose failure surfaces inside the envelope, not as a worker
        // fault).
        assert_eq!(
            row.status, "completed",
            "child {i} worker should reach a completed lifecycle; lines: {lines:?}"
        );
        if i == 2 {
            // The failing child: ok normalized to false, error surfaced.
            assert!(
                !row.ok,
                "failing child (index 2) must have ok == false; lines: {lines:?}"
            );
            assert!(
                row.error_present,
                "failing child (index 2) must surface a non-nil error; lines: {lines:?}"
            );
        } else {
            // Siblings unaffected: still ok, still carry their own marker.
            assert!(row.ok, "sibling {i} should still be ok; lines: {lines:?}");
            assert!(
                !row.error_present,
                "sibling {i} should have no error; lines: {lines:?}"
            );
            assert_eq!(
                row.summary,
                format!("MARK-{i}"),
                "sibling {i} cross-talk under partial failure; lines: {lines:?}"
            );
        }
    }
    eprintln!(
        "PARTIAL-FAILURE ISOLATION CONFIRMED: child 2 ok=false+error, 4 siblings ok+own-marker"
    );
}

#[test]
fn fanout_runs_all_waves_without_dropping_or_reordering() {
    // Five requests with max_parallel: 2 → waves [2, 2, 1]. All five must
    // come back, in input order, each with its own marker.
    let source = format!(
        r#"{PRELUDE}
pipeline main(task) {{
  let labels = ["w0", "w1", "w2", "w3", "w4"]
  var reqs = []
  var i = 0
  for label in labels {{
    let marker = "WAVE-" + to_string(i)
    reqs = reqs.push({{task: "do " + marker, options: base_opts(ok_caller(marker)), label: label}})
    i = i + 1
  }}
  let results = agent_fanout(reqs, {{max_parallel: 2}})
  log("COUNT=" + to_string(len(results)))
  emit_rows(results)
}}
"#
    );

    let raw = run_with_bridge(&source).expect("fanout pipeline must run");
    let lines = out_lines(&raw);
    eprintln!("--- harn output ---\n{}", lines.join("\n"));

    let rows = parse_rows(&lines);
    let labels = ["w0", "w1", "w2", "w3", "w4"];
    assert_eq!(
        rows.len(),
        labels.len(),
        "wave chunking dropped/added results; lines: {lines:?}"
    );

    for (i, expected_label) in labels.iter().enumerate() {
        let row = &rows[i];
        assert_eq!(row.index, i, "wave reordering at {i}; lines: {lines:?}");
        assert_eq!(&row.label, expected_label, "lines: {lines:?}");
        assert!(row.ok, "wave child {i} should be ok; lines: {lines:?}");
        assert_eq!(
            row.summary,
            format!("WAVE-{i}"),
            "wave child {i} returned the wrong marker; lines: {lines:?}"
        );
    }
    eprintln!("WAVES CONFIRMED: 5 requests across waves of 2 all completed in input order with own markers");
}

#[test]
fn fanout_isolates_a_spawn_time_throw() {
    // A spawn can throw SYNCHRONOUSLY — before any worker handle exists —
    // when a request's `options` are malformed. Here index 1 ("bravo") carries
    // an `allowed_tools` list with a non-string entry, so `sub_agent_run` ->
    // `sub_agent_request` -> `__sub_agent_string_list` throws inside the wave
    // spawn loop. The fix must turn that into THAT unit's ok:false result
    // (status "failed", error surfaced) without aborting the wave — the whole
    // `agent_fanout` call must NOT throw, and the siblings must still return
    // their own markers in input order.
    let source = format!(
        r#"{PRELUDE}
pipeline main(task) {{
  let labels = ["alpha", "bravo", "charlie"]
  var reqs = []
  var i = 0
  for label in labels {{
    let marker = "MARK-" + to_string(i)
    let opts = if i == 1 {{
      base_opts(ok_caller(marker)) + {{allowed_tools: [123]}}
    }} else {{
      base_opts(ok_caller(marker))
    }}
    reqs = reqs.push({{task: "do " + marker, options: opts, label: label}})
    i = i + 1
  }}
  let results = agent_fanout(reqs, {{max_parallel: 8}})
  log("COUNT=" + to_string(len(results)))
  emit_rows(results)
}}
"#
    );

    // The whole call must NOT throw — `.expect` here is itself the regression
    // guard: before the fix, the spawn throw propagated out of `agent_fanout`.
    let raw = run_with_bridge(&source).expect("a spawn-time throw must not abort agent_fanout");
    let lines = out_lines(&raw);
    eprintln!("--- harn output ---\n{}", lines.join("\n"));

    let rows = parse_rows(&lines);
    let labels = ["alpha", "bravo", "charlie"];
    assert_eq!(
        rows.len(),
        labels.len(),
        "spawn throw dropped sibling results; lines: {lines:?}"
    );

    for (i, expected_label) in labels.iter().enumerate() {
        let row = &rows[i];
        assert_eq!(row.index, i, "lines: {lines:?}");
        assert_eq!(&row.label, expected_label, "lines: {lines:?}");
        if i == 1 {
            // The unit whose spawn threw: synthetic failure result, error
            // surfaced, never ran an LLM turn (no marker summary).
            assert!(
                !row.ok,
                "spawn-failed unit (index 1) must have ok == false; lines: {lines:?}"
            );
            assert!(
                row.error_present,
                "spawn-failed unit (index 1) must surface the spawn fault; lines: {lines:?}"
            );
            assert_eq!(
                row.status, "failed",
                "spawn-failed unit (index 1) status should be 'failed'; lines: {lines:?}"
            );
            assert_eq!(
                row.summary, "",
                "spawn-failed unit (index 1) never produced an envelope summary; lines: {lines:?}"
            );
        } else {
            // Siblings unaffected: still ok, still carry their own marker.
            assert!(row.ok, "sibling {i} should still be ok; lines: {lines:?}");
            assert!(
                !row.error_present,
                "sibling {i} should have no error; lines: {lines:?}"
            );
            assert_eq!(
                row.summary,
                format!("MARK-{i}"),
                "sibling {i} cross-talk / drop under a spawn throw; lines: {lines:?}"
            );
        }
    }
    eprintln!(
        "SPAWN-THROW ISOLATION CONFIRMED: index 1 spawn fault -> ok=false+error, siblings ok+own-marker, no wave abort"
    );
}

#[test]
fn fanout_spawn_throw_in_first_wave_does_not_drop_later_waves() {
    // Four requests with max_parallel: 2 -> waves [0,1], [2,3]. Index 0's spawn
    // throws (malformed allowed_tools) in the FIRST wave. Before the fix that
    // throw aborted the wave AND every later wave, silently dropping waves 2.
    // The fix must let later waves run: all four results come back, in order,
    // with only index 0 marked failed.
    let source = format!(
        r#"{PRELUDE}
pipeline main(task) {{
  let labels = ["w0", "w1", "w2", "w3"]
  var reqs = []
  var i = 0
  for label in labels {{
    let marker = "WAVE-" + to_string(i)
    let opts = if i == 0 {{
      base_opts(ok_caller(marker)) + {{allowed_tools: [123]}}
    }} else {{
      base_opts(ok_caller(marker))
    }}
    reqs = reqs.push({{task: "do " + marker, options: opts, label: label}})
    i = i + 1
  }}
  let results = agent_fanout(reqs, {{max_parallel: 2}})
  log("COUNT=" + to_string(len(results)))
  emit_rows(results)
}}
"#
    );

    let raw =
        run_with_bridge(&source).expect("a first-wave spawn throw must not abort later waves");
    let lines = out_lines(&raw);
    eprintln!("--- harn output ---\n{}", lines.join("\n"));

    let rows = parse_rows(&lines);
    let labels = ["w0", "w1", "w2", "w3"];
    assert_eq!(
        rows.len(),
        labels.len(),
        "a first-wave spawn throw dropped later waves; lines: {lines:?}"
    );

    for (i, expected_label) in labels.iter().enumerate() {
        let row = &rows[i];
        assert_eq!(row.index, i, "wave reordering at {i}; lines: {lines:?}");
        assert_eq!(&row.label, expected_label, "lines: {lines:?}");
        if i == 0 {
            assert!(
                !row.ok,
                "spawn-failed wave child 0 must be ok=false; lines: {lines:?}"
            );
            assert!(
                row.error_present,
                "spawn-failed wave child 0 must surface an error; lines: {lines:?}"
            );
            assert_eq!(row.status, "failed", "lines: {lines:?}");
        } else {
            // The crucial assertion: wave-2 children (index 2,3) still ran.
            assert!(
                row.ok,
                "later-wave child {i} should be ok; lines: {lines:?}"
            );
            assert_eq!(
                row.summary,
                format!("WAVE-{i}"),
                "later-wave child {i} returned the wrong marker / was dropped; lines: {lines:?}"
            );
        }
    }
    eprintln!(
        "LATER-WAVES-SURVIVE CONFIRMED: wave-1 spawn throw at index 0 did not drop wave-2 children"
    );
}
