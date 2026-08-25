# Coming from LangChain

You know how to build an agent. This page maps the LangChain pieces you already
reach for onto their Harn equivalents so you can port a working idea rather than
relearn the vocabulary.

For LangGraph specifically — `Node`, `Edge`, `State`, `Channel`, `super-step`,
`interrupt` — the vocabulary table in
[Coming from elsewhere](../concepts/sota-comparison.md#langgraph) is the
cross-reference. This page covers the LangChain side and the day-to-day
mechanics.

| LangChain | Harn | Where |
| --- | --- | --- |
| LCEL `a \| b \| c` | the pipe operator, `x \|> f(_) \|> g(_)` | [below](#composition) |
| `StateGraph` | `workflow_graph` + `workflow_execute`, or a portable workflow bundle | [below](#graphs-and-workflows) |
| `@tool` | an inline `tool` declaration, or `harn tool new` for a shareable one | [below](#tools) |
| `with_structured_output(Model)` | `output: {schema: T}` on the call | [below](#structured-output) |
| `RunnableRetry` / `.with_retry()` | `with_retry` from `std/llm/handlers` | [below](#retries-and-middleware) |
| LangSmith | `harn portal` and `harn usage`, local and included | [below](#tracing-and-cost) |
| `checkpointer` | `checkpoint_stage` from `std/checkpoint` | [below](#resuming-after-a-crash) |

## Composition

LCEL's `|` chains runnables left to right. Harn's pipe does the same for
ordinary functions, with one difference: the placeholder is explicit, so the
piped value can land in any argument position rather than only the first.

```harn,check
fn double(x: int) -> int {
  return x * 2
}

fn add(x: int, y: int) -> int {
  return x + y
}

fn main(harness: Harness) {
  const out = 3 |> double(_) |> add(_, 1)
  harness.stdio.log(to_string(out))  // 7
}
```

The reasoning behind the explicit `_` is in
[ADR-0001](../adr/0001-pipe-operator.md). Note that Harn's pipe composes plain
function calls; it is not a separate runnable protocol with its own streaming
and batching semantics. Model calls, retries, and fallbacks compose through
[middleware](#retries-and-middleware) instead.

## Graphs and workflows

A `StateGraph` becomes a Harn workflow. There are two shapes, and which one you
want depends on who runs it.

For a graph your own program runs, build it with `workflow_graph` and execute it
in-process with `workflow_execute`. Nodes are stages, edges carry a branch
label, and retries and verification are node policy rather than hand-rolled
loops. See [the workflow runtime](../workflow-runtime.md).

For a graph a *host* runs — an IDE, an orchestrator — author a portable workflow
bundle instead. It is JSON, it validates to a stable `graph_digest`, and the
host executes it. See
[Run a workflow bundle from the CLI](../workflow-authoring-quickstart.md).

The one thing that does not map cleanly is LangGraph's typed state with
reducers. Harn's default state model is workflow artifacts and the transcript,
not a typed dict merged per super-step. That gap and its design are described in
the [LangGraph table](../concepts/sota-comparison.md#langgraph).

## Tools

`@tool` decorates a Python function. The everyday Harn equivalent is a `tool`
declaration inside the program that uses it — typed parameters, a description
written for the model, and the body inline:

```harn,ignore
tool search(pattern: string) -> string {
  description "Search the project"
  return harness.process.exec("rg", "--", pattern).stdout ?? ""
}
```

When the tool should be shared across projects rather than living in one
program, scaffold it as a package:

```bash
harn tool new summarize-diff --description "Summarize a git diff"
```

See [Extend Harn](../extend-harn.md) for how tools relate to packages,
connectors, and skills.

## Structured output

`with_structured_output(Model)` binds a Pydantic model to a call. Harn takes a
type on the call's `output` option, and the same option controls provider-level
strictness, post-parse validation, and early stream abort:

```harn,ignore
type Verdict = {pass: bool, reason: string}

const result = harness.llm.call(prompt, nil, {
  output: {
    schema: Verdict,
    strict: true,
    validation: "error",
    stream_abort: true,
  },
  schema_retries: 1,
})
```

`result.data` is the narrowed value. `schema_retries` re-asks the model when
parsing fails, which is the behavior you would otherwise write around a
LangChain output parser. Full option list in
[`harness.llm.call`](../llm/llm_call.md).

## Retries and middleware

`RunnableRetry` wraps a runnable. Harn wraps the *call*: `agent_loop` accepts an
`llm_caller` closure that owns each turn's `harness.llm.call`, and the handlers
in `std/llm/handlers` compose around it.

```harn,ignore
import {default_llm_caller} from "std/llm/caller"
import {with_retry} from "std/llm/handlers"

const caller = with_retry(default_llm_caller(), {max_attempts: 4})

const result = agent_loop(harness, task, system, {
  loop_until_done: true,
  llm_caller: caller,
})
```

Prefer `llm_caller({retry: {max_attempts: 4}})` for the blessed default stack —
it is the same composition plus typed reserved-status classification and
billed-empty re-dispatch. Fallback, shadow, logging, budget, cache, and circuit
breaker are sibling handlers you compose the same way. See
[the handlers catalog](../stdlib/llm-handlers.md).

## Tracing and cost

LangSmith is a hosted service you sign up for. The Harn equivalents run on your
machine and are part of the toolchain:

```bash
harn portal        # observability UI over persisted runs
harn usage         # spend and token rollups from the local event log
```

`harn portal` binds `127.0.0.1:4721` by default and reads run records from
`.harn-runs/`. `harn usage` aggregates `provider_call_response` events out of
the project's `.harn/events.sqlite` and rolls them up by provider, model, or a
day/week/month series — it reuses the cost the runtime already computed rather
than re-pricing anything. Neither needs an account or sends data anywhere.

See [Debugging agent runs](../debugging.md) and [`harn usage`](../usage.md).

## Resuming after a crash

A LangGraph checkpointer persists graph state so a thread can resume. Harn's
closest everyday primitive is `checkpoint_stage`, which caches a stage's result
under a name and skips the work on a resumed run:

```harn,check
import { checkpoint_stage } from "std/checkpoint"

fn main(harness: Harness) {
  const data = checkpoint_stage(harness.runtime, "fetch", { -> "raw" })
  const cleaned = checkpoint_stage(
    harness.runtime, "clean", { -> data + "-clean" },
  )
  harness.stdio.log(cleaned)
}
```

The first argument is `harness.runtime` — the capability that owns the
checkpoint store. `checkpoint_stage_keyed` adds an identity so the same stage
name can be checkpointed per item, and the `_retry` variants add bounded
retries.

For an agent that parks and resumes rather than crashes and restarts, the
mechanism is different: see
[the daemon agent tutorial](../tutorial-daemon-agent.md) for
`agent_await_resumption`, worker snapshots, and `harn run --resume`.

## What has no direct equivalent

- **A retriever abstraction.** Harn has no `VectorStoreRetriever` interface.
  Retrieval is something you write as a tool or a stage.
- **A document loader ecosystem.** There is no `langchain-community` analogue.
- **Chain serialization.** LCEL chains do not serialize to a portable format;
  Harn's portable unit is a workflow bundle or a signed `.harnpack`, which is a
  different granularity.
