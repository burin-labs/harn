# Harn

Harn is a pipeline-oriented programming language for orchestrating AI agents.
LLM calls, tool use, concurrency, and error recovery are built into the
language instead of spread across SDK glue.

```harn
let response = llm_call(
  "Explain quicksort in two sentences.",
  "You are a computer science tutor."
)
println(response)
```

Harn files can contain top-level code like the above (implicit pipeline),
or organize logic into named pipelines for larger programs:

```harn
pipeline default(task) {
  let files = ["src/main.rs", "src/lib.rs"]

  let reviews = parallel each files { file ->
    let content = read_file(file)
    llm_call("Review this code:\n${content}", "You are a code reviewer.")
  }

  for review in reviews {
    println(review)
  }
}
```

## Get started

Start with [Getting started](./getting-started.md): install Harn, write a
program, and run it in under five minutes.

## What's in this guide

- [Getting started](./getting-started.md): install and run your first program
- [Why Harn?](./why-harn.md): what Harn solves and how it compares
- [Language basics](./language-basics.md): syntax, types, control flow, functions, structs, enums
- [Error handling](./error-handling.md): try/catch, Result type, the `?` operator, retry
- [Modules and imports](./modules.md): files, modules, and the standard library
- [Concurrency](./concurrency.md): spawn/await, parallel, channels, mutexes, deadlines
- [Language specification](./language-spec.md): grammar and runtime semantics
- [LLM and agents](./llm-and-agents.md): model calls, agent loops, and tool use
- [Transcript architecture](./transcript-architecture.md): storage and replay for agent conversations
- [Workflow runtime](./workflow-runtime.md): workflow graphs, artifacts, run records, replay, evals
- [Cookbook](./cookbook.md): practical recipes and patterns
- [Host boundary](./host-boundary.md): integration with host applications
- [Bridge protocol](./bridge-protocol.md): JSON-RPC contract for host bridges
- [Protocol support matrix](./protocol-support.md): ACP, A2A, and MCP entry points
- [MCP, ACP, and A2A integration](./mcp-and-acp.md): protocol examples and behavior
- [Harn portal](./portal.md): local observability UI for runs and transcripts
- [CLI reference](./cli-reference.md): commands and flags
- [Builtin functions](./builtins.md): built-in function reference
- [Editor integration](./editor-integration.md): LSP, tree-sitter, and formatter support
- [Testing](./testing.md): user tests and the conformance suite

## Links

- [GitHub](https://github.com/burin-labs/harn)
- [Language specification](./language-spec.md)
