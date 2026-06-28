<!-- markdownlint-disable MD013 MD033 MD041 -->
<div class="harn-hero">

<h1 class="harn-hero-title">Harn</h1>

<p class="tagline">A pipeline-oriented language for AI agent orchestration. LLM calls, tool use, concurrency, retries, and replay are built into the runtime.</p>

```harn
let response = llm_call(
  "Explain quicksort in two sentences.",
  "You are a computer science tutor."
)
log(response)
```

<div class="harn-cta-row">
<a class="harn-cta harn-cta-primary" href="./getting-started.html">Get started</a>
<a class="harn-cta harn-cta-secondary" href="./concepts/index.html">Read the concepts</a>
<a class="harn-cta harn-cta-secondary" href="https://github.com/burin-labs/harn">GitHub</a>
</div>

</div>

## What Harn is

Harn is a small language built around one observation: when you write an AI agent, most of your code is *coordination* — calling a model, dispatching a tool, retrying, fanning out work, persisting state, recovering from a crash, replaying a trace for debugging. Harn gives those patterns one runtime and one syntax surface.

```harn
pipeline review(task) {
  let files = ["src/main.rs", "src/lib.rs"]

  let reviews = parallel each files { file ->
    let code = read_file(file)
    retry 3 {
      llm_call(code, "Review this Rust file and list any issues.")
    }
  }

  for review in reviews {
    log(review)
  }
}
```

The orchestration logic is the code: which work fans out, which calls retry,
where results join, and which effects pass through the harness.

## Why it exists

<div class="harn-feature-grid">

<div class="harn-feature">

### LLM calls as primitives

`llm_call`, `agent_loop`, and `workflow_execute` are part of the language. Switch providers with a one-field change.

</div>

<div class="harn-feature">

### Concurrency with structure

`parallel each`, `spawn`/`await`, channels, and deadlines keep fan-out, joins,
and cancellation visible in the program.

</div>

<div class="harn-feature">

### Replay built in

Every agent loop produces a structured transcript that replays deterministically. Debug the run that happened, not your reconstruction of it.

</div>

<div class="harn-feature">

### Protocols out of the box

Native MCP, ACP, and A2A. Expose your pipeline as an agent backend, or call any MCP server with three lines.

</div>

<div class="harn-feature">

### Suspend and resume

Cooperatively pause a worker mid-loop, persist a snapshot, resume hours later with new input. Daemon agents are first-class.

</div>

<div class="harn-feature">

### Gradually typed

Annotations are optional. Add them where they help, leave them off where they don't. Structural shape types describe expected dict fields.

</div>

</div>

## Three paths in

<div class="harn-paths">

<div class="harn-path-card">

### Build something

Start with [Getting started](./getting-started.md) for install + first program in five minutes. Then [a code-review agent tutorial](./tutorial-code-review-agent.md).

</div>

<div class="harn-path-card">

### Understand the model

Read the [Concepts](./concepts/index.md) section: how `llm_call`, `agent_loop`, `workflow`, and `pipeline` fit together, and when to reach for each.

</div>

<div class="harn-path-card">

### Coming from elsewhere

If you're arriving from agent SDKs or orchestrators like LangGraph, Inngest, or Mastra, use the [terminology reference](./concepts/sota-comparison.md) to get up to speed.

</div>

</div>

## Where to go next

- [Tutorials](./getting-started.md) — install, write your first program, then walk through worked examples.
- [How-to guides](./cookbook.md) — task-oriented recipes for the things you actually need to do.
- [Reference](./builtins.md) — every builtin, option, and CLI flag.
- [Explanation](./host-boundary.md) — architecture notes, protocol RFCs, ADRs.

## Links

- [GitHub repository](https://github.com/burin-labs/harn)
- [Language specification](./language-spec.md)
- [Feature matrix vs Inngest, Temporal, LangGraph, Cursor Automations](./feature-matrix.md)
