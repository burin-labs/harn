# Harn

Harn is a pipeline-oriented programming language for orchestrating AI coding agents. It has native LLM calls, tool use, structured output, and async concurrency built into the language.

```
pipeline default(task) {
  let tools = tool_registry()
    |> tool_add("search", "Search the web", search_fn, {query: "string"})

  let result = llm_call(task, "You are a research assistant", {
    tools: tools,
    response_format: "json",
  })

  log(result.data)
}
```

## Getting started

### Prerequisites

Harn is built with Rust. You'll need:

- **[Rust](https://rustup.rs/)** (1.70 or later) — install with `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Git**

### Install from source

```bash
git clone https://github.com/burin-labs/harn
cd harn && cargo build --release
cp target/release/harn ~/.local/bin/
```

Create a project and run it:

```bash
harn init my-agent
cd my-agent
export ANTHROPIC_API_KEY=sk-...
harn run main.harn
```

## What's in this guide

- **[Why Harn?](./why-harn.md)** -- What problems Harn solves and how it compares to existing approaches
- **[Language Basics](./language-basics.md)** -- Syntax, types, control flow, functions, structs, enums
- **[Error Handling](./error-handling.md)** -- try/catch, Result type, the `?` operator, retry
- **[Modules and Imports](./modules.md)** -- Splitting code across files, standard library
- **[Concurrency](./concurrency.md)** -- spawn/await, parallel, channels, mutexes, deadlines
- **[LLM Calls and Agent Loops](./llm-and-agents.md)** -- Calling models, agent loops, tool use
- **[Builtin Functions](./builtins.md)** -- Complete reference for all built-in functions
- **[Cookbook](./cookbook.md)** -- Practical recipes and patterns

## Links

- [GitHub](https://github.com/burin-labs/harn)
- [Language Specification](https://github.com/burin-labs/harn/blob/main/spec/HARN_SPEC.md)
