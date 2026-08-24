<!-- markdownlint-disable MD033 -->

# Harn

Harn is a programming language and runtime for building AI agents. Model calls,
tools, retries, concurrency, transcripts, and workflows are language and
standard-library features, so programs need less orchestration glue.

```harn,check title="example.harn"
fn main(harness: Harness) {
  const response = harness.llm.call(
    "Explain quicksort in two sentences.",
    "You are a computer science tutor.",
    { provider: "mock" }
  )
  harness.stdio.println(response.text)
}
```

That runs with no API key: the `mock` provider is deterministic and offline.
[Getting started](./getting-started.md) installs Harn and runs it.

## When Harn helps

Harn helps when you want agent behavior to read like a program: which model
runs, when a tool fires, how a failure is handled, and what the run records.
You do not need experience with an agent framework. Familiarity with one
programming language or with large language models (LLMs) will make some terms
feel familiar, but it is not required.

For one model call, an existing SDK may be enough. Harn becomes more useful as
your program gains tools, retries, multiple providers, concurrency, replay, or
long-running work. Harn also includes a [portal](./portal.md) for inspecting
persisted runs.

## Building blocks

Harn supports one model request, an agent loop, or a multi-stage workflow.
Start with the smallest building block that fits the task:

| Need | Start with |
|---|---|
| One request and one response | [`harness.llm.call`](./llm/llm_call.md) |
| A model that can use tools across turns | [`agent_loop`](./llm/agent_loop.md) |
| Named stages with joins and retries | [`workflow_execute`](./workflow-runtime.md) |

Put any of these in [`fn main`](./language-basics.md) or a named
[pipeline](./language-basics.md#pipelines). Add a larger abstraction only when
the program needs it. [The expressiveness spectrum](./concepts/expressiveness-spectrum.md)
shows the same task as a model call, an agent loop, and a workflow.

## Where to go next

- [Why Harn?](./why-harn.md) explains the design with the same program in
  Python and Harn.
- [Coming from elsewhere](./concepts/sota-comparison.md) maps Harn terms to
  other agent tools and protocols.
- The [feature matrix](./how-harn-compares.md) compares runtime guarantees across
  Harn, Inngest, Temporal, LangGraph, and Cursor Automations.
- The [mental model](./concepts/mental-model.md) shows how Harn's parts fit.
- [Common tasks](./common-tasks.md) starts from a goal you want to complete.

Harn owns the reusable agent behavior: orchestration, model and tool calls,
transcripts, replay and evaluation, worker lineage, and capability policy. Your
application keeps its own interface, approval flow, file changes, and product
data. In practice, you write the steps the agent should take. The Harn virtual
machine (VM) handles provider adapters, retries, transcripts, and runtime
policy. [The host boundary](./host-boundary.md) explains the full split.

## Harn at a glance

<dl class="fact-panel">
  <dt>Paradigm</dt>
  <dd>Pipeline-oriented, imperative, with structured concurrency</dd>
  <dt>Typing</dt>
  <dd>
    <a href="./spec/language/19-type-annotations.md">Gradual and structural</a>.
    Annotations are optional everywhere.
  </dd>
  <dt>Implemented in</dt>
  <dd>Rust, as a lexer, parser, type checker, and tree-walking VM</dd>
  <dt>Runs on</dt>
  <dd>
    macOS and Linux on Intel and ARM, Windows on x86-64.
    <a href="./platform-support.md">Platform support</a> has the detail.
  </dd>
  <dt>File extensions</dt>
  <dd>
    <code>.harn</code> for programs.
    <code>.harn.prompt</code> for prompt templates.
  </dd>
  <dt>Speaks</dt>
  <dd>
    <a href="./mcp-and-acp.md">MCP, ACP, and A2A</a>, natively
  </dd>
  <dt>License</dt>
  <dd>MIT or Apache-2.0, at your option</dd>
  <dt>Maturity</dt>
  <dd>
    Pre-1.0. Surface-level breaking changes are possible between minor and
    patch releases. See the
    <a href="https://github.com/burin-labs/harn/blob/main/CHANGELOG.md">changelog</a>.
  </dd>
</dl>

## Links

- [Language specification](./language-spec.md)
- [GitHub repository](https://github.com/burin-labs/harn)
