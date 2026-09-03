//! The shipped binary must survive a stop on Tokio's *default* worker stack.
//!
//! Every Rust test lane here exports `RUST_MIN_STACK=16777216`, which raises
//! the ambient default for every thread the process spawns — including Tokio's
//! worker threads. That is why the existing background-stop coverage in
//! `conformance/tests/agents/agent_stop_graceful_handoff.harn` ran green for
//! five releases while the same work aborted a customer's process (harn#7961):
//! in-process coverage cannot see this class at all.
//!
//! So this test spawns the real binary with `RUST_MIN_STACK` removed from the
//! child environment. The child then gets whatever a shipped `harn` gets, and
//! a worker thread that has not stated its stack size overflows. A stack
//! overflow is not a catchable panic: the process aborts, so the assertion is
//! on the child's exit status and on the absence of the abort message, not on
//! any output the run would have produced.

use std::process::Command;

/// A background sub-agent that does real tool work, stopped mid-flight.
///
/// The tool calls matter. The frames that overflow are the agent loop's, and a
/// sub-agent that only produces text finishes too shallow to reach them.
const SCRIPT: &str = r#"
import { agent_stop, sub_agent_run, wait_agent } from "std/agent/workers"
import { llm_text, with_llm_script } from "std/testing"

fn slow_tools(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "wait_a_bit",
    "Sleep briefly",
    {
      handler: { args -> harness.clock.sleep_ms(400)
        return "waited" },
      parameters: {},
      returns: {type: "string"},
    },
  )
  return tools
}

pipeline main(harness: Harness) {
  with_llm_script(
    harness.llm,
    [
      {tool_calls: [{id: "a1", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a2", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a3", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a4", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a5", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a6", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a7", name: "wait_a_bit", arguments: {}}]},
      {tool_calls: [{id: "a8", name: "wait_a_bit", arguments: {}}]},
      llm_text("done"),
    ],
    { ->
      const handle = sub_agent_run(
        harness,
        "Call wait_a_bit repeatedly.",
        {
          provider: "mock",
          background: true,
          tools: slow_tools(harness),
          allowed_tools: ["wait_a_bit"],
          tool_format: "native",
          max_iterations: 12,
        },
      )
      harness.clock.sleep_ms(900)
      const stopped = agent_stop(
        harness.agent,
        handle,
        {graceful: true, reason: "operator pressed stop"},
      )
      harness.stdio.println("stop_reason=" + to_string(stopped?.handoff?.metadata?.stop_reason))
      const final = wait_agent(harness.agent, handle)
      harness.stdio.println("final_status=" + to_string(final?.status))
    },
  )
}
"#;

#[test]
fn stopping_a_background_sub_agent_survives_the_default_worker_stack() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("stop_on_default_stack.harn");
    std::fs::write(&script, SCRIPT).expect("write script");

    // A reused state directory is not a rerun of this test. A second run in
    // the same directory resumes worker state and takes a shallower path, so
    // it completes on the default stack even when the defect is present. The
    // fresh HOME is what keeps a pass meaningful.
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .arg("run")
        .arg(&script)
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env_remove("RUST_MIN_STACK")
        .output()
        .expect("run harn");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("overflowed its stack"),
        "a Tokio worker overflowed on the default stack; a shipped multi-thread \
         runtime is missing `.thread_stack_size(harn_vm::RUNTIME_STACK_SIZE)`.\n\
         stderr:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "harn exited {:?} on the default stack\nstderr:\n{stderr}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("final_status=stopped"),
        "the stop did not reach a terminal state\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
