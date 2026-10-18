# LLM calls and agents

Use the smallest API that matches the job:

| Job | API | What it does |
|---|---|---|
| One model response | `harness.llm.call` | Sends one request and returns one response. |
| Several model/tool turns | `agent_loop` | Keeps working until it reaches a terminal state or a limit. |
| Named dependent stages | `workflow_execute` | Runs a typed, inspectable workflow graph. |
| Independent child work | Workers | Runs separate agent contexts that a parent can await or resume. |

See [Configure a model provider](./provider-setup.md) before you use a real
provider. Use `provider: "mock"` in examples and tests that should run without
credentials.

## One model call

```harn
import { LlmCallOptions } from "std/llm/options"

fn main(harness: Harness) {
  const options: LlmCallOptions = {
    provider: "mock",
    max_tokens: 128,
  }
  const response = harness.llm.call(
    "Translate 'Hello, world' to French.",
    "You are a concise translator.",
    options,
  )
  harness.stdio.println(response.text)
}
```

`harness.llm.call` returns a canonical response. Read `text` for the answer;
read `usage`, `outcome`, `tool_calls`, and `transcript` when your program needs
accounting or run information. See [LLM calls](./llm/llm_call.md) for the full
return shape, structured output, streaming, and errors.

## An agent loop

Use a loop when the model must choose actions across several turns. Build the
options as an `AgentSpec` so `harn check` can catch misspelled options.

```harn
import { agent_loop } from "std/agent/loop"
import { AgentSpec } from "std/agent/options"

fn main(harness: Harness) {
  const options: AgentSpec = {
    provider: "mock",
    loop_until_done: true,
    max_iterations: 2,
  }
  const result = agent_loop(
    harness,
    "Answer this question in one sentence: why test a program?",
    "You are a concise teacher.",
    options,
  )
  harness.stdio.println(result.status)
  harness.stdio.println(result.text)
}
```

An agent result includes the visible text, terminal outcome, model usage, tool
summary, and transcript. Only a natural terminal outcome proves that the agent
completed its task. See [Agent loops](./llm/agent_loop.md) for tools, budgets,
sessions, workers, and suspension.

## Record preparation with an agent run

Use `harness.agent.with_run(session_id, options, body)` when model calls prepare
an agent's task or tools. It starts the durable journal before the callback.
The callback's first `agent_loop` on that session adopts the same run, including
the preparatory calls' usage and reasoning receipts.

```harn
import { agent_loop } from "std/agent/loop"

fn main(harness: Harness) {
  harness.llm.mock_enqueue({text: "Explain why programs need tests."})
  harness.llm.mock_enqueue({text: "Tests expose changes in behavior."})
  const session = harness.agent.open()
  const options = {
    provider: "mock",
    session_id: session,
    loop_until_done: false,
  }
  const result = harness.agent.with_run(session, options, { ->
    const task = harness.llm.call(
      "Describe the next task briefly.",
      nil,
      options + {call_role: "application.classifier"},
    )
    agent_loop(harness, task.text, nil, options)
  })
  harness.stdio.println(result.status)
}
```

The recording options accept the same store root, session lineage, and workflow
learning provenance as an agent loop. An explicit `options.session_id` must
match `session_id`; mismatches are refused before the callback runs. The loop
must use the same store root. Admission denials retain any preparatory calls in
both the returned physical usage and the durable run.
Its workflow learning provenance governs subsequent events. A callback that
returns before starting a loop receives a completed terminal; one that throws
receives a failed terminal. Cancellation uses the same durable cleanup as the
loop. Each new scope starts a separate run on the continuing session. A second
scope on a session with an active journal is refused.

## Workflows and workers

Use a workflow when the program has named stages, dependencies, joins, or
verification steps. Use a worker when a parent must delegate independent or
long-running work. These are orchestration choices; the model call remains the
smallest unit inside them.

- [Workflow runtime](./workflow-runtime.md)
- [Delegated workers](./llm/agent_loop.md#delegated-workers)
- [Typed tools](./llm/tools.md)
- [Streaming and transcripts](./llm/streaming.md)

## Provider choice

Harn resolves a provider from call options, project configuration, environment,
or the model catalog. For reproducible programs, set the provider explicitly
in the options and use a current model from `harn models list`.

The [provider reference](./llm/providers.md) contains endpoint and capability
details. The [provider setup guide](./provider-setup.md) contains the shortest
path from an API key to a verified call.
