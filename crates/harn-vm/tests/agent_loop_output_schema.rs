#![recursion_limit = "256"]
//! Coverage for `agent_loop`'s terminal `output_schema` gate.
//!
//! `output_schema` promises the loop's FINAL answer parses against a schema.
//! It is a terminal-answer contract, not a per-turn one — the loop never
//! forces mid-loop turns into structured mode (that would fight tool-calling).
//! At finalize (only on a `"done"` completion) the loop validates the terminal
//! answer; if off-shape it re-asks ONCE, through the same `llm_caller` seam
//! ordinary turns use, for a schema-shaped value. The parsed value is surfaced
//! on `run.output` and `run.output_valid` records whether validation passed.
//!
//! These tests stub the LLM via the `llm_caller` seam (mirroring
//! `agent_loop_final_wrapup.rs`) and count repair calls / inspect the surfaced
//! `output`/`output_valid` to prove:
//!   (a) an off-shape terminal answer re-asks once and yields a valid output;
//!   (b) an already-valid terminal answer fires NO repair call;
//!   (c) a persistently off-shape answer yields `output_valid: false`, no crash;
//!   (d) with `output_schema` UNSET the run carries no `output`/`output_valid`;
//!   (e) tool-calling mid-loop still works with `output_schema` set.

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

/// Terminal answer is off-shape prose; the one repair re-ask returns a
/// schema-shaped value. Logs: status, repair-call count, output_valid,
/// output.answer.
#[test]
fn off_shape_terminal_answer_reasks_once_and_validates() {
    let raw = run_with_bridge(
        r#"
pipeline main(task) {
  let session = "output-schema-repair"
  let repair_counter = shared_cell(
    {scope: "task_group", key: "os-repair-" + session, initial: 0},
  )
  let schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  let mock_llm = { _call ->
    if _call?.opts?._output_schema_repair == true {
      let snap = shared_snapshot(repair_counter)
      shared_cas(repair_counter, snap, snap.value + 1)
      return {ok: true, value: {text: "{\"answer\": 42}", provider: "mock", model: "mock"}}
    }
    return {
      ok: true,
      value: {
        text: "<user_response>The answer is roughly forty-two.</user_response>\n<done>FINISHED</done>",
        provider: "mock",
        model: "mock",
      },
    }
  }
  let result = agent_loop(
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      output_schema: schema,
      llm_caller: mock_llm,
    },
  )
  log(result.status)
  log(shared_get(repair_counter))
  log(result.output_valid)
  log(result.output.answer)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "expected exactly one repair call; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "true",
        "expected output_valid true; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "42",
        "expected output.answer == 42; lines: {lines:?}"
    );
}

/// Terminal answer is already schema-valid JSON — no repair call fires.
#[test]
fn already_valid_terminal_answer_fires_no_repair() {
    let raw = run_with_bridge(
        r#"
pipeline main(task) {
  let session = "output-schema-direct"
  let repair_counter = shared_cell(
    {scope: "task_group", key: "os-direct-" + session, initial: 0},
  )
  let schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  let mock_llm = { _call ->
    if _call?.opts?._output_schema_repair == true {
      let snap = shared_snapshot(repair_counter)
      shared_cas(repair_counter, snap, snap.value + 1)
    }
    return {
      ok: true,
      value: {
        text: "<user_response>{\"answer\": 7}</user_response>\n<done>FINISHED</done>",
        provider: "mock",
        model: "mock",
      },
    }
  }
  let result = agent_loop(
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      output_schema: schema,
      llm_caller: mock_llm,
    },
  )
  log(result.status)
  log(shared_get(repair_counter))
  log(result.output_valid)
  log(result.output.answer)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "0",
        "expected zero repair calls; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "true",
        "expected output_valid true; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "7",
        "expected output.answer == 7; lines: {lines:?}"
    );
}

/// Persistently off-shape (both the terminal answer and the repair re-ask
/// fail to parse): `output_valid` is false and the loop does not crash.
#[test]
fn persistently_off_shape_yields_output_valid_false() {
    let raw = run_with_bridge(
        r#"
pipeline main(task) {
  let session = "output-schema-fail"
  let schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  let mock_llm = { _call ->
    return {
      ok: true,
      value: {
        text: "<user_response>Still just prose, no JSON here.</user_response>\n<done>FINISHED</done>",
        provider: "mock",
        model: "mock",
      },
    }
  }
  let result = agent_loop(
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      output_schema: schema,
      llm_caller: mock_llm,
    },
  )
  log(result.status)
  log(result.output_valid)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "false",
        "expected output_valid false; lines: {lines:?}"
    );
}

/// With `output_schema` unset the run carries no `output`/`output_valid`
/// keys — behavior is byte-unchanged for callers that never opt in.
#[test]
fn unset_output_schema_leaves_run_unchanged() {
    let raw = run_with_bridge(
        r#"
pipeline main(task) {
  let session = "output-schema-unset"
  let mock_llm = { _call ->
    return {
      ok: true,
      value: {
        text: "<user_response>All done.</user_response>\n<done>FINISHED</done>",
        provider: "mock",
        model: "mock",
      },
    }
  }
  let result = agent_loop(
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      llm_caller: mock_llm,
    },
  )
  log(result.status)
  log(result?.output_valid == nil)
  log(result?.output == nil)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "true",
        "expected no output_valid key; lines: {lines:?}"
    );
    assert_eq!(lines[2], "true", "expected no output key; lines: {lines:?}");
}

/// Tool-calling mid-loop still works when `output_schema` is set: the model
/// dispatches a tool first, then completes with a schema-valid answer. The
/// tool fires (terminal-only gating does not block it) and the output validates.
#[test]
fn tool_calling_still_works_with_output_schema_set() {
    let raw = run_with_bridge(
        r#"
pipeline main(task) {
  clear_tool_hooks()
  let session = "output-schema-tools"
  let registry = tool_registry()
  let tools = tool_define(
    registry,
    "lookup",
    "Return a fixed value.",
    {parameters: {}, handler: { _args -> return "looked up" }},
  )
  let tool_counter = shared_cell(
    {scope: "task_group", key: "os-tools-" + session, initial: 0},
  )
  let schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  let mock_llm = { _call ->
    if _call?.opts?._output_schema_repair == true {
      return {ok: true, value: {text: "{\"answer\": 99}", provider: "mock", model: "mock"}}
    }
    let snap = shared_snapshot(tool_counter)
    let n = snap.value
    shared_cas(tool_counter, snap, n + 1)
    if n == 0 {
      return {
        ok: true,
        value: {
          text: "",
          tool_calls: [{id: "call_lookup", name: "lookup", arguments: {}}],
          provider: "mock",
          model: "mock",
        },
      }
    }
    return {
      ok: true,
      value: {
        text: "<user_response>The lookup returned the value.</user_response>\n<done>FINISHED</done>",
        tool_calls: [],
        provider: "mock",
        model: "mock",
      },
    }
  }
  let result = agent_loop(
    "do the work",
    nil,
    {
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 5,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      session_id: session,
      output_schema: schema,
      llm_caller: mock_llm,
    },
  )
  log(result.status)
  log(contains(result.tools.successful, "lookup"))
  log(result.output_valid)
  log(result.output.answer)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "true",
        "expected the lookup tool to have fired; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "true",
        "expected output_valid true; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "99",
        "expected output.answer == 99; lines: {lines:?}"
    );
}
