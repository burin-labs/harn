# Migrate to the single agent plane

This migration removes public wrappers that owned overlapping loop, chat, and
editor-completion behavior. Migrate each call to the capability that owns its
lifecycle.

## Replace `AgentLoopOptions`

Rename the annotation to `AgentSpec`. The fields remain flat, so existing
values need no reshaping.

```harn
import { AgentSpec, agent_options } from "std/agent/options"

const spec: AgentSpec = agent_options({
  provider: "openai",
  model: "gpt-5-mini",
  loop_until_done: true,
})
```

Use the component records when a function accepts only part of the contract:
`AgentModelSpec`, `AgentExecutionSpec`, `AgentCapabilitySpec`,
`AgentLifecycleSpec`, `AgentContextSpec`, and `AgentObservabilitySpec`.

## Replace `agent_turn`

Call `agent_loop` and set completion policy explicitly. Use `turn_end_condition` when a
judge must approve completion.

```harn
const result = agent_loop(harness, task, system, agent_options({
  provider: "openai",
  model: "gpt-5-mini",
  loop_until_done: true,
  turn_end_condition: true,
}))
```

Read judge decisions from `judge_decision` session events. The removed wrapper's
separate `iterations` and `judge_decisions` result summaries are not part of
`AgentResult`.

## Replace `agent_llm_turn`

Use `harness.llm.call(prompt, system?, options?)`. One request belongs to
`HarnessLlm`; adding an agent-named wrapper does not make it an agent loop.

## Replace `agent_chat_loop`

Keep input handling, slash commands, and presentation in the host. Invoke
`agent_loop` once per prompt turn. Pass `history` when the host owns conversation
storage, or reuse a `session_id` when the Harn session owns it.

```harn
const result = agent_loop(harness, message, system, agent_options({
  session_id: conversation_id,
  history: stored_messages,
  tools: tools,
}))
```

Use typed HITL or `agent_await_resumption` for suspension. Do not recreate the
removed `wait_for_user` convention as a terminal string.

## Move editor completions to the host

`std/agent/completions` is removed. Editors own cursor state, suggestion UI,
acceptance decisions, and product telemetry. Call `harness.llm.completion` for
one completion request and store host-specific envelopes in the product layer.

## Update result handling

Annotate loop results with `AgentResult` from `std/agent/contracts`. Branch on
`result.terminal.kind`; retain `stop_reason` only for diagnostics.

`agent_loop` no longer returns an untyped nullable dictionary. Remove defensive
optional access on the result itself (`result?.status` becomes `result.status`);
keep optional access only for fields whose declared type is optional.

If a reader imports `AgentResult` from `std/agent/artifacts`, rename that
annotation to `AgentResultArtifact`. The artifact type remains compatible with
persisted v1/v2 files; `AgentResult` now names only the live loop contract from
`std/agent/contracts`.

```harn
import { AgentResult } from "std/agent/contracts"

const result: AgentResult = agent_loop(harness, task, system, spec)
if result.terminal.kind == "natural" {
  return result.visible_text
}
throw "agent stopped: " + result.terminal.kind
  + ": " + result.terminal.reason
```
