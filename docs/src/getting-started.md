# Getting started

This page gets you from zero to running your first Harn program.

## Prerequisites

- **[Rust](https://rustup.rs/)** 1.70 or later -- install with
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Git**

## Installation

### From crates.io

```bash
cargo install harn-cli
```

### From source

```bash
git clone https://github.com/burin-labs/harn
cd harn && cargo build --release
cp target/release/harn ~/.local/bin/
```

Verify the installation:

```bash
harn version
```

## Your first program

Create a file called `hello.harn`:

```harn
println("Hello, world!")
```

Run it:

```bash
harn run hello.harn
```

That's it. Harn files can contain top-level code without any boilerplate.
The above is an **implicit pipeline** -- the runtime wraps your top-level
statements automatically.

## Adding a pipeline

For larger programs, organize code into named pipelines. The runtime
executes the `default` pipeline (or the first one declared):

```harn
pipeline default(task) {
  let name = "Harn"
  println("Hello from ${name}!")
}
```

The `task` parameter is injected by the host runtime. It carries the
user's request when Harn is used as an agent backend.

## Calling an LLM

Harn has native LLM support. Set your API key and call a model directly:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

```harn
let response = llm_call(
  "Explain quicksort in two sentences.",
  "You are a computer science tutor."
)
println(response)
```

No imports, no SDK initialization, no response parsing. Harn ships with
built-in configs for Anthropic, OpenAI, OpenRouter, Ollama, HuggingFace,
and local OpenAI-compatible servers.

## The REPL

Start an interactive session:

```bash
harn repl
```

The REPL evaluates expressions as you type and displays results
immediately. Useful for experimenting with builtins and small snippets.

## Project setup

Scaffold a new project with `harn init`:

```bash
harn init my-agent
cd my-agent
```

This creates a directory with `harn.toml` (project config) and
`main.harn` (entry point). Run it with:

```bash
harn run main.harn
```

## Remote MCP quick start

If you want to use a cloud MCP server such as Notion, authorize it once with
the CLI and then reference it from `harn.toml`:

```bash
harn mcp redirect-uri
harn mcp login https://mcp.notion.com/mcp --scope "read write"
```

## Next steps

- **[Why Harn?](./why-harn.md)** -- What problems Harn solves
- **[Language basics](./language-basics.md)** -- Syntax, types, control flow
- **[LLM calls and agent loops](./llm-and-agents.md)** -- Calling models and building agents
- **[Cookbook](./cookbook.md)** -- Practical recipes and patterns
