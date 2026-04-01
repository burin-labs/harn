# Harn

Harn is a pipeline-oriented programming language for orchestrating AI
coding agents. LLM calls, tool use, concurrency, and error recovery are
built into the language -- no libraries or SDKs needed.

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

  let reviews = parallel_map(files) { file ->
    let content = read_file(file)
    llm_call("Review this code:\n" + content, "You are a code reviewer.")
  }

  for review in reviews {
    println(review)
  }
}
```

## Get started

The fastest way to start is the [Getting Started](./getting-started.md)
guide: install Harn, write a program, and run it in under five minutes.

## What's in this guide

- **[Getting Started](./getting-started.md)** -- Install and run your first program
- **[Why Harn?](./why-harn.md)** -- What problems Harn solves and how it compares
- **[Language Basics](./language-basics.md)** -- Syntax, types, control flow, functions, structs, enums
- **[Error Handling](./error-handling.md)** -- try/catch, Result type, the `?` operator, retry
- **[Modules and Imports](./modules.md)** -- Splitting code across files, standard library
- **[Concurrency](./concurrency.md)** -- spawn/await, parallel, channels, mutexes, deadlines
- **[LLM Calls and Agent Loops](./llm-and-agents.md)** -- Calling models, agent loops, tool use
- **[Workflow Runtime](./workflow-runtime.md)** -- Workflow graphs, artifacts, run records, replay, evals
- **[MCP and ACP Integration](./mcp-and-acp.md)** -- MCP client/server, ACP, and A2A protocols
- **[CLI Reference](./cli-reference.md)** -- All CLI commands and flags
- **[Builtin Functions](./builtins.md)** -- Complete reference for all built-in functions
- **[Language Specification](./language-spec.md)** -- Formal grammar and runtime semantics
- **[Cookbook](./cookbook.md)** -- Practical recipes and patterns

## Links

- [GitHub](https://github.com/burin-labs/harn)
- [Language Specification](./language-spec.md)
