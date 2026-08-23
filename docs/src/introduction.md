<!-- markdownlint-disable MD033 -->

# Harn

Harn is a programming language and runtime for building AI agents. Model calls,
tools, retries, concurrency, transcripts, and workflows are language and
standard-library features rather than libraries you assemble yourself.

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

## Is this for you?

Harn is worth your time if you are writing the orchestration yourself and want
it to be readable: which model runs, when a tool fires, what happens on
failure, and what got recorded. It expects you have written code in another
language, and assumes nothing about which one.

It is probably not what you want if you need a hosted product with a dashboard,
or if a single model call from an existing SDK already does the job.

## Choose the smallest useful building block

| Need | Start with |
|---|---|
| One request and one response | [`harness.llm.call`](./llm/llm_call.md) |
| A model that can use tools across turns | [`agent_loop`](./llm/agent_loop.md) |
| Named stages with joins and retries | [`workflow_execute`](./workflow-runtime.md) |
| A complete Harn program | [`fn main`](./language-basics.md) and [pipelines](./language-basics.md#pipelines) |

You are never forced up that table, and moving up is never a rewrite. [The
expressiveness spectrum](./concepts/expressiveness-spectrum.md) walks one task
from three lines to a full workflow to show what each rung costs.

## Where to go next

- **Why does this exist?** [Why Harn?](./why-harn.md) makes the argument, with
  the same program written in Python and in Harn.
- **I already use something else.** [Coming from
  elsewhere](./concepts/sota-comparison.md) maps Harn's vocabulary onto the
  OpenAI and Anthropic SDKs, LangGraph, Flue, Inngest, Mastra, and the MCP, ACP,
  and A2A specs.
- **How does it compare on capabilities?** [Feature
  matrix](./feature-matrix.md) puts Harn beside Inngest, Temporal, LangGraph,
  and Cursor Automations, and says where each guarantee actually lives.
- **How do the pieces fit?** [Mental model](./concepts/mental-model.md), then
  [Concepts](./concepts/index.md).
- **I know what I want to build.** [Common tasks](./common-tasks.md).

Harn owns the reusable agent behavior: orchestration, model and tool calls,
transcripts, replay and evaluation, worker lineage, and capability policy. Your
application keeps its own interface, approval flow, file changes, and product
data. [The host boundary](./host-boundary.md) is the full split.

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
    Pre-1.0. Surface-level breaking changes are possible between minor
    releases. See the
    <a href="https://github.com/burin-labs/harn/blob/main/CHANGELOG.md">changelog</a>.
  </dd>
</dl>

## Links

- [Language specification](./language-spec.md)
- [GitHub repository](https://github.com/burin-labs/harn)
