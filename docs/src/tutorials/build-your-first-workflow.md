# Build your first workflow

By the end of this page you will have built an agent that fixes a failing test
and checks its own work. You will start with a single model call, grow it into
an agent that can use tools, and finish with a two-stage workflow that verifies
the result and retries with feedback when the check fails.

No prior agent-orchestration background is assumed. Every block on this page is a
complete program you can save and run. We use the built-in `mock` provider so
everything runs offline with no API key. The `mock` provider returns
canned text, so it won't actually repair code, but it lets you watch the whole
machine wire up and run. When you want a model that does real work, change one
word (`provider`) and read [LLM providers](../llm/providers.md).

## What you need

A working `harn` binary. Check it:

```bash
harn --version
```

That's the only prerequisite. The `mock` provider ships in the binary.

## Step 1: one model call

The smallest useful thing is a single request. Save this as `fix.harn`:

```harn
let answer = llm_call(
  "The test test_add expects add(2, 2) == 4 but got 5. What's the likely bug?",
  "You are a careful Rust engineer.",
  {provider: "mock"},
)
log(answer.text)
```

Run it:

```bash
harn run fix.harn
```

`llm_call` sends one prompt and returns [a result dict](../llm/llm_call.md#return-value).
The text is in `answer.text`; token counts, the model name, and the full
transcript are on the same dict when you need them.

One call, one answer. There is no loop and no way for the model to look at a
file or run the test. That is the right tool when the whole job is "read this,
tell me that." Ours needs more: to fix the bug, the agent has to read the code,
edit it, run the test, and react to what the test says. That is a loop.

## Step 2: an agent that uses tools

`agent_loop` runs a model in a loop: it calls the model, dispatches any tools
the model asked for, feeds the results back, and repeats until the model reports
it's done. You give it a task, a system prompt, and a set of tools. It owns the
loop, the budget, and deciding when "done" means done.

```harn
let result = agent_loop(
  "Fix the failing test test_add in src/math.rs, then run the test to confirm.",
  "You are a senior engineer. Make the smallest change that turns the test green.",
  {provider: "mock", loop_until_done: true},
)
log(result.status)          // "done", "stuck", "budget_exhausted", ...
log(result.text)            // the agent's final output
log(result.llm.iterations)  // how many model round-trips it took
```

`loop_until_done: true` tells the loop to keep going until the model signals
completion rather than stopping after the first turn. The
[`status`](../llm/agent_loop.md) tells you how it ended: `done` when the model
finished cleanly, `stuck` or `budget_exhausted` when it ran out of road.

On the `mock` provider this returns almost immediately with canned text. On a
real provider you would also pass `tools:` so the agent can read and edit files
and run the test. See [Agent tools](../llm/tools.md#default-mutation-tools) for
the ready-made `write_file` / `edit_file` / `run` set; the shape is:

```harn,ignore
import { agent_edit_tools } from "std/agent/host_tools"

let result = agent_loop(task, system, {
  provider: "ollama",
  model: "qwen3-coder",
  tools: agent_edit_tools(),
  loop_until_done: true,
})
```

One `agent_loop` is one goal, run to completion. It's the right rung for "make
this one thing happen." But notice what it doesn't do: nothing outside the model
checks whether the test actually passes at the end. The agent decides it's done;
we take its word. For real work you want an independent gate, and a way to hand
the agent another attempt when the gate says no. That is a workflow.

## Step 3: verify the result

A workflow runs stages in order. Each stage is a step: an agent doing work, or a
plain command that checks the work. Here we wire two stages: an `act` stage that
tries the fix, and a `verify` stage that runs the test and reads the exit code.

```harn
import { workflow_stages } from "std/workflow/patterns"

let graph = workflow_stages({
  name: "fix-the-test",
  stages: [
    {id: "act", kind: "stage", mode: "agent", model_policy: {provider: "mock"}},
    {id: "check", kind: "verify", mode: "command",
      verify: {command: "cargo test test_add --quiet", expect_status: 0}},
  ],
})

let run = workflow_execute("Fix the failing test test_add.", graph)
log(run.status)
log(run.path)
```

The `act` stage is an agent, like Step 2. The `check` stage runs a real command
and passes only when it exits `0`. The verifier is separate from the agent, so
"the agent thinks it's done" and "the test is actually green" are two different
facts, decided by two different things. That separation is the entire point of
lifting to a workflow.

`workflow_execute` runs the graph and writes a run record to `run.path`. Inspect
it with:

```bash
harn runs view --json <run.path>
```

Right now, if `check` fails, the workflow stops. The agent gets one shot. The
last piece is giving it another shot, with the failure in hand.

## Step 4: retry with feedback

Add a `retry_policy` to the acting stage. Two keys turn a single attempt into a
repair loop: `max_attempts` caps how many tries the stage gets, and
`feedback: true` threads the previous failure's findings into the next attempt's
prompt. The agent's second try starts with the first try's error, not a blank
slate.

```harn
import { workflow_stages } from "std/workflow/patterns"

let graph = workflow_stages({
  name: "fix-the-test",
  stages: [
    {id: "act", kind: "stage", mode: "agent",
      model_policy: {provider: "mock"},
      retry_policy: {max_attempts: 3, feedback: true}},
    {id: "check", kind: "verify", mode: "command",
      verify: {command: "cargo test test_add --quiet", expect_status: 0}},
  ],
})

let run = workflow_execute("Fix the failing test test_add.", graph)
log(run.status)
```

Now the stage tries up to three times. After a failed attempt, the next prompt
carries `Previous attempt N failed: <findings>`, where the findings are the
verifier's output. The agent reads what went wrong and adjusts. It stops the
moment an attempt passes, and every attempt is recorded in the run so you can
replay exactly what it tried.

That is the self-verifying agent from the top of the page: it does the work,
an independent check grades it, and a failed grade becomes the input to the next
attempt.

On the `mock` provider the canned reply never actually fixes the code, so you'll
watch all three attempts run and the stage exhaust its retries, the machine
working exactly as designed. Point `model_policy` at a real model and give the
`act` stage edit and run tools, and the same graph fixes the test for real.

## What you built, and where to go next

You climbed the three rungs of the [abstraction ladder](../concepts/abstraction-ladder.md):

- `llm_call` — one request, one answer.
- `agent_loop` — one goal, run to completion with tools.
- `workflow_stages` — more than one attempt, with an independent verify gate and
  retry-with-feedback.

The rule that keeps you on the right rung: use the lowest one that covers the
job, and lift only when the *shape* of the work changes: a second goal, a
verify stage, a retry loop, a different model per stage.

From here:

- [Choosing an agent abstraction](../concepts/abstraction-ladder.md) — the full
  ladder and when each rung earns its cost.
- [The expressiveness spectrum](../concepts/expressiveness-spectrum.md) — the
  same task solved at five escalating levels of control, from a three-line call
  up to a hand-tuned workflow.
- [Workflow runtime](../workflow-runtime.md) — the reference for stages, verify
  nodes, retry policy, and the executor closure.
- [LLM providers](../llm/providers.md) — swap `mock` for Ollama, Anthropic, or
  OpenAI.
