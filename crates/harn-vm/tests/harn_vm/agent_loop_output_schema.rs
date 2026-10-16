//! Coverage for `agent_loop`'s terminal `output` gate.
//!
//! `output` promises the loop's FINAL answer parses against a schema.
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
//!   (d) with `output` UNSET the run carries no `output`/`output_valid`;
//!   (e) tool-calling mid-loop still works with `output` set.

use crate::support::output_lines as out_lines;

fn run_with_bridge(source: &str) -> Result<String, String> {
    let source = format!(
        "import {{ agent_loop }} from \"std/agent/loop\"\nimport {{ sub_agent_run }} from \"std/agent/workers\"\n{source}"
    );
    crate::support::run_with_host_bridge(&source)
}

/// Terminal answer is off-shape prose; the one repair re-ask returns a
/// schema-shaped value. Logs: status, repair-call count, output_valid,
/// output.answer.
#[test]
fn off_shape_terminal_answer_reasks_once_and_validates() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-repair"
  const repair_counter = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-repair-" + session, initial: 0},
  )
  const schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  const mock_llm = { _call ->
    if _call?.opts?._output_contract_repair == true {
      const snap = harness.runtime.shared_snapshot(repair_counter)
      harness.runtime.shared_cas(repair_counter, snap, snap.value + 1)
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
  const result = agent_loop(
    harness,
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      output: schema,
      llm_caller: mock_llm,
    },
  )
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(repair_counter))
  harness.stdio.log(result.output_valid)
  harness.stdio.log(result.output.answer)
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
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-direct"
  const repair_counter = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-direct-" + session, initial: 0},
  )
  const call_counter = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-direct-calls-" + session, initial: 0},
  )
  const schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  const mock_llm = { _call ->
    const call_snapshot = harness.runtime.shared_snapshot(call_counter)
    harness.runtime.shared_cas(call_counter, call_snapshot, call_snapshot.value + 1)
    if _call?.opts?._output_contract_repair == true {
      const snap = harness.runtime.shared_snapshot(repair_counter)
      harness.runtime.shared_cas(repair_counter, snap, snap.value + 1)
    } else {
      assert_eq(_call?.opts?.output, nil, "ordinary turns must keep native tools available")
      assert(
        contains(to_string(_call?.system ?? ""), json_stringify(schema)),
        "the ordinary request must teach the declared schema before any repair",
      )
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
  const result = agent_loop(
    harness,
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      output: schema,
      llm_caller: mock_llm,
    },
  )
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(repair_counter))
  harness.stdio.log(result.output_valid)
  harness.stdio.log(result.output.answer)
  harness.stdio.log(harness.runtime.shared_get(call_counter))
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
    assert_eq!(
        lines[4], "1",
        "natural completion needs one call; lines: {lines:?}"
    );
}

/// Persistently off-shape (both the terminal answer and the repair re-ask
/// fail to parse): `output_valid` is false and the loop does not crash.
#[test]
fn persistently_off_shape_yields_output_valid_false() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-fail"
  const schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  const mock_llm = { _call ->
    return {
      ok: true,
      value: {
        text: "<user_response>Still just prose, no JSON here.</user_response>\n<done>FINISHED</done>",
        provider: "mock",
        model: "mock",
      },
    }
  }
  const result = agent_loop(
    harness,
    "answer",
    nil,
    {
      provider: "mock",
      max_iterations: 3,
      loop_until_done: true,
      done_sentinel: "FINISHED",
      tool_format: "text",
      session_id: session,
      output: schema,
      llm_caller: mock_llm,
    },
  )
  harness.stdio.log(result.status)
  harness.stdio.log(result.output_valid)
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

/// With `output` unset the run carries no `output`/`output_valid`
/// keys — behavior is byte-unchanged for callers that never opt in.
#[test]
fn unset_output_leaves_run_unchanged() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-unset"
  const mock_llm = { _call ->
    return {
      ok: true,
      value: {
        text: "<user_response>All done.</user_response>\n<done>FINISHED</done>",
        provider: "mock",
        model: "mock",
      },
    }
  }
  const result = agent_loop(
    harness,
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
  harness.stdio.log(result.status)
  harness.stdio.log(result?.output_valid == nil)
  harness.stdio.log(result?.output == nil)
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

/// Tool-calling mid-loop still works when `output` is set: the model
/// dispatches a tool first, then completes with a schema-valid answer. The
/// tool fires (terminal-only gating does not block it) and the output validates.
#[test]
fn tool_calling_still_works_with_output_set() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  harness.tools.clear_hooks()
  const session = "output-schema-tools"
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "lookup",
    "Return a fixed value.",
    {parameters: {}, handler: { _args -> return "looked up" }},
  )
  const tool_counter = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-tools-" + session, initial: 0},
  )
  const schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  const mock_llm = { _call ->
    if _call?.opts?._output_contract_repair == true {
      return {ok: true, value: {text: "{\"answer\": 99}", provider: "mock", model: "mock"}}
    }
    const snap = harness.runtime.shared_snapshot(tool_counter)
    const n = snap.value
    harness.runtime.shared_cas(tool_counter, snap, n + 1)
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
  const result = agent_loop(
    harness,
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
      output: schema,
      llm_caller: mock_llm,
    },
  )
  harness.stdio.log(result.status)
  harness.stdio.log(contains(result.tools.successful, "lookup"))
  harness.stdio.log(result.output_valid)
  harness.stdio.log(result.output.answer)
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

#[test]
fn exhausted_wrapup_returns_schema_checked_partial_output_without_repair() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-exhausted-wrapup"
  const wrapup_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-wrapup-" + session, initial: 0},
  )
  const ordinary_schema_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-ordinary-schema-" + session, initial: 0},
  )
  const wrapup_schema_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-wrapup-schema-" + session, initial: 0},
  )
  const schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  const caller = { call ->
    if call?.opts?._final_wrapup == true {
      const snapshot = harness.runtime.shared_snapshot(wrapup_calls)
      harness.runtime.shared_cas(wrapup_calls, snapshot, snapshot.value + 1)
      if call?.opts?.output?.properties?.answer?.type == "int" {
        const schema_snapshot = harness.runtime.shared_snapshot(wrapup_schema_calls)
        harness.runtime.shared_cas(wrapup_schema_calls, schema_snapshot, schema_snapshot.value + 1)
      }
      return {
        ok: true,
        value: {
          text: "{\"answer\":42}",
          provider: "mock",
          model: "mock",
        },
      }
    }
    if call?.opts?.output != nil {
      const snapshot = harness.runtime.shared_snapshot(ordinary_schema_calls)
      harness.runtime.shared_cas(ordinary_schema_calls, snapshot, snapshot.value + 1)
    }
    return {ok: true, value: {text: "Still working.", provider: "mock", model: "mock"}}
  }
  const result = agent_loop(harness, "do the work", nil, {
    provider: "mock",
    max_iterations: 1,
    loop_until_done: true,
    turn_end_condition: false,
    session_id: session,
    output: schema,
    wrapup_directive: "Return the final decision as JSON.",
    llm_caller: caller,
  })
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(wrapup_calls))
  harness.stdio.log(harness.runtime.shared_get(wrapup_schema_calls))
  harness.stdio.log(harness.runtime.shared_get(ordinary_schema_calls))
  harness.stdio.log(result.output_valid)
  harness.stdio.log(result.output.answer)
  harness.stdio.log(result.publication_admitted)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    assert_eq!(lines[1], "1", "expected one wrap-up; lines: {lines:?}");
    assert_eq!(
        lines[2], "1",
        "the wrap-up must receive the schema; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "0",
        "ordinary turns must omit the schema; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "true",
        "valid wrap-up data must be retained; lines: {lines:?}"
    );
    assert_eq!(
        lines[5], "42",
        "typed value must reach the terminal result; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "false",
        "partial output must not admit publication; lines: {lines:?}"
    );
}

#[test]
fn sub_agent_tool_turn_keeps_schema_checked_wrapup_data_after_budget_exhaustion() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "sub-agent-output-schema-exhausted"
  const tool_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "sub-agent-output-tool-" + session, initial: 0},
  )
  const ordinary_schema_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "sub-agent-output-ordinary-schema-" + session, initial: 0},
  )
  const wrapup_schema_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "sub-agent-output-wrapup-schema-" + session, initial: 0},
  )
  const json_protocol_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "sub-agent-output-json-protocol-" + session, initial: 0},
  )
  const tagged_protocol_calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "sub-agent-output-tagged-protocol-" + session, initial: 0},
  )
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "read_only_probe",
    "Read a fixed test value.",
    {parameters: {}, handler: {_args ->
      const snapshot = harness.runtime.shared_snapshot(tool_calls)
      harness.runtime.shared_cas(tool_calls, snapshot, snapshot.value + 1)
      return "observed"
    }},
  )
  const schema = {
    type: "object",
    properties: {verdict: {type: "string", enum: ["confirmed"]}},
    required: ["verdict"],
    additionalProperties: false,
  }
  const caller = {call ->
    if call?.opts?._final_wrapup == true {
      const system = to_string(call?.system ?? "")
      if contains(system, "ONLY a JSON value matching the declared output contract") {
        const snapshot = harness.runtime.shared_snapshot(json_protocol_calls)
        harness.runtime.shared_cas(json_protocol_calls, snapshot, snapshot.value + 1)
      }
      if contains(system, "Emit a concise `<user_response>") {
        const snapshot = harness.runtime.shared_snapshot(tagged_protocol_calls)
        harness.runtime.shared_cas(tagged_protocol_calls, snapshot, snapshot.value + 1)
      }
      if call?.opts?.output?.schema?.properties?.verdict != nil {
        const snapshot = harness.runtime.shared_snapshot(wrapup_schema_calls)
        harness.runtime.shared_cas(wrapup_schema_calls, snapshot, snapshot.value + 1)
      }
      return {
        ok: true,
        value: {
          text: "The release saves `memory + {phase: phase}`.\n\n{\"verdict\":\"confirmed\"}",
          provider: "mock",
          model: "mock",
          input_tokens: 7,
          output_tokens: 5,
        },
      }
    }
    if call?.opts?.output != nil {
      const snapshot = harness.runtime.shared_snapshot(ordinary_schema_calls)
      harness.runtime.shared_cas(ordinary_schema_calls, snapshot, snapshot.value + 1)
    }
    return {
      ok: true,
      value: {
        text: "",
        tool_calls: [{id: "read_probe", name: "read_only_probe", arguments: {}}],
        provider: "mock",
        model: "mock",
        input_tokens: 11,
        output_tokens: 3,
      },
    }
  }
  const result = sub_agent_run(harness, "read a value and return it", {
    provider: "mock",
    model: "mock",
    session_id: session,
    tools: tools,
    tool_format: "native",
    max_iterations: 1,
    loop_until_done: true,
    output: "json",
    returns: {schema: schema},
    llm_caller: caller,
  })
  harness.stdio.log(result.status)
  harness.stdio.log(result.ok)
  harness.stdio.log(result.error.category)
  harness.stdio.log(result.data.verdict)
  harness.stdio.log(harness.runtime.shared_get(tool_calls))
  harness.stdio.log(harness.runtime.shared_get(wrapup_schema_calls))
  harness.stdio.log(harness.runtime.shared_get(ordinary_schema_calls))
  harness.stdio.log(harness.runtime.shared_get(json_protocol_calls))
  harness.stdio.log(harness.runtime.shared_get(tagged_protocol_calls))
  harness.stdio.log(result.tokens_used)
  harness.stdio.log(result.usage.input_tokens)
  harness.stdio.log(result.usage.output_tokens)
  harness.stdio.log(result.usage.total_tokens)
  harness.stdio.log(result.usage.provider_call_count)
  harness.stdio.log(result.usage.token_scope)
  harness.stdio.log(result.usage.status)
}
"#,
    )
    .expect("sub-agent must execute through the VM");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    assert_eq!(
        lines[1], "false",
        "partial output is not completion; lines: {lines:?}"
    );
    assert_eq!(lines[2], "policy_budget", "lines: {lines:?}");
    assert_eq!(
        lines[3], "confirmed",
        "typed partial output must remain inspectable; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "1",
        "the read-only tool must actually fire; lines: {lines:?}"
    );
    assert_eq!(
        lines[5], "1",
        "the one wrap-up carries the schema; lines: {lines:?}"
    );
    assert_eq!(
        lines[6], "0",
        "ordinary tool turns omit the schema; lines: {lines:?}"
    );
    assert_eq!(
        lines[7], "1",
        "wrap-up prompt requests only JSON; lines: {lines:?}"
    );
    assert_eq!(
        lines[8], "0",
        "schema-bound wrap-up omits response tags; lines: {lines:?}"
    );
    assert_eq!(
        lines[9], "0",
        "mock callers have no physical provider usage; lines: {lines:?}"
    );
    assert_eq!(
        lines[10], "0",
        "mock callers do not create physical input usage; lines: {lines:?}"
    );
    assert_eq!(
        lines[11], "0",
        "mock callers do not create physical output usage; lines: {lines:?}"
    );
    assert_eq!(
        lines[12], "0",
        "physical receipt total is a measured zero; lines: {lines:?}"
    );
    assert_eq!(
        lines[13], "0",
        "no provider request reached a physical transport; lines: {lines:?}"
    );
    assert_eq!(
        lines[14], "every_provider_call",
        "zero usage declares its physical-call scope; lines: {lines:?}"
    );
    assert_eq!(
        lines[15], "not_run",
        "zero usage is explicitly marked as no provider call; lines: {lines:?}"
    );
}

#[test]
fn sub_agent_invalid_exhausted_output_preserves_budget_failure_envelope() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "sub-agent-output-schema-invalid-exhausted"
  const calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "sub-agent-invalid-output-calls-" + session, initial: 0},
  )
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "read_only_probe",
    "Read a fixed test value.",
    {parameters: {}, handler: {_args -> return "observed"}},
  )
  const schema = {
    type: "object",
    properties: {verdict: {type: "string", enum: ["confirmed"]}},
    required: ["verdict"],
    additionalProperties: false,
  }
  const caller = {call ->
    const snapshot = harness.runtime.shared_snapshot(calls)
    harness.runtime.shared_cas(calls, snapshot, snapshot.value + 1)
    if call?.opts?._final_wrapup == true {
      return {ok: true, value: {text: "{\"verdict\":\"unknown\"}", provider: "mock", model: "mock"}}
    }
    return {
      ok: true,
      value: {
        text: "",
        tool_calls: [{id: "read_probe", name: "read_only_probe", arguments: {}}],
        provider: "mock",
        model: "mock",
      },
    }
  }
  const result = sub_agent_run(harness, "read a value and return it", {
    provider: "mock",
    model: "mock",
    session_id: session,
    tools: tools,
    tool_format: "native",
    max_iterations: 1,
    loop_until_done: true,
    returns: {schema: schema},
    llm_caller: caller,
  })
  harness.stdio.log(result.status)
  harness.stdio.log(result.ok)
  harness.stdio.log(result.error.category)
  harness.stdio.log(result.data == nil)
  harness.stdio.log(harness.runtime.shared_get(calls))
}
"#,
    )
    .expect("sub-agent must execute through the VM");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    assert_eq!(
        lines[1], "false",
        "invalid partial data cannot succeed; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "policy_budget",
        "schema failure must preserve budget cause; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "true",
        "invalid schema output must not populate data; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "2",
        "only main turn and one wrap-up may run; lines: {lines:?}"
    );
}

#[test]
fn empty_exhausted_wrapup_cannot_reuse_prior_json_or_repair() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-empty-completed-wrapup"
  const calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-empty-wrapup-calls-" + session, initial: 0},
  )
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "read_only_probe",
    "Read a fixed test value.",
    {parameters: {}, handler: {_args -> return "observed"}},
  )
  const schema = {
    type: "object",
    properties: {answer: {type: "int"}},
    required: ["answer"],
  }
  const caller = {call ->
    const snapshot = harness.runtime.shared_snapshot(calls)
    harness.runtime.shared_cas(calls, snapshot, snapshot.value + 1)
    if call?.opts?._final_wrapup == true {
      return {ok: true, value: {text: "", provider: "mock", model: "mock"}}
    }
    return {
      ok: true,
      value: {
        text: "{\"answer\":42}",
        tool_calls: [{id: "read_probe", name: "read_only_probe", arguments: {}}],
        provider: "mock",
        model: "mock",
      },
    }
  }
  const result = agent_loop(harness, "answer", nil, {
    provider: "mock",
    tools: tools,
    tool_format: "native",
    max_iterations: 1,
    loop_until_done: true,
    turn_end_condition: false,
    done_sentinel: "FINISHED",
    session_id: session,
    output: schema,
    wrapup_directive: "Return the final decision as JSON.",
    llm_caller: caller,
  })
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(calls))
  harness.stdio.log(result.output_valid == nil)
  harness.stdio.log(result.output == nil)
  harness.stdio.log(result.publication_admitted)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    assert_eq!(
        lines[1], "2",
        "only the main turn and wrap-up may run; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "false",
        "the attempted empty wrap-up is invalid; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "true",
        "prior JSON cannot stand in for the wrap-up; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "false",
        "empty wrap-up cannot admit publication; lines: {lines:?}"
    );
}

#[test]
fn invalid_exhausted_wrapup_cannot_reuse_prior_json_or_repair() {
    let raw = run_with_bridge(
        r#"
pipeline main(harness: Harness, task: unknown) {
  const session = "output-schema-invalid-exhausted-wrapup"
  const calls = harness.runtime.shared_cell(
    {scope: "task_group", key: "os-invalid-calls-" + session, initial: 0},
  )
  const schema = {
    type: "object",
    properties: {verdict: {type: "string", enum: ["confirmed"]}},
    required: ["verdict"],
    additionalProperties: false,
  }
  const caller = { call ->
    const snapshot = harness.runtime.shared_snapshot(calls)
    harness.runtime.shared_cas(calls, snapshot, snapshot.value + 1)
    if call?.opts?._final_wrapup == true {
      return {ok: true, value: {text: "{\"verdict\":\"unknown\"}", provider: "mock", model: "mock"}}
    }
    return {ok: true, value: {text: "{\"verdict\":\"confirmed\"}", provider: "mock", model: "mock"}}
  }
  const result = agent_loop(harness, "do the work", nil, {
    provider: "mock",
    max_iterations: 1,
    loop_until_done: true,
    turn_end_condition: false,
    session_id: session,
    output: schema,
    wrapup_directive: "Return the final decision as JSON.",
    llm_caller: caller,
  })
  harness.stdio.log(result.status)
  harness.stdio.log(harness.runtime.shared_get(calls))
  harness.stdio.log(result.output_valid)
  harness.stdio.log(result.output == nil)
  harness.stdio.log(result.publication_admitted)
}
"#,
    )
    .expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "budget_exhausted", "lines: {lines:?}");
    assert_eq!(
        lines[1], "2",
        "expected one main call and one wrap-up; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "false",
        "invalid wrap-up schema must fail closed; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "true",
        "invalid output must not escape as typed data; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "false",
        "invalid partial data cannot admit publication; lines: {lines:?}"
    );
}
