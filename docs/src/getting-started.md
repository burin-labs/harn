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
cd harn
./scripts/dev_setup.sh   # installs dev tooling, portal deps/build, git hooks, sccache
cargo build --release
cp target/release/harn ~/.local/bin/
```

Verify the installation:

```bash
harn version
```

Optional shell completions:

```bash
mkdir -p ~/.local/share/bash-completion/completions
harn completions bash > ~/.local/share/bash-completion/completions/harn

mkdir -p ~/.zfunc
harn completions zsh > ~/.zfunc/_harn
# Add to ~/.zshrc if needed: fpath=(~/.zfunc $fpath); autoload -Uz compinit; compinit

mkdir -p ~/.config/fish/completions
harn completions fish > ~/.config/fish/completions/harn.fish
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

Harn has native LLM support. Run quickstart to inspect available provider
credentials, local Ollama status, disk space, and GPU availability, then write
starter `harn.toml`, `providers.toml`, and `.env` files:

```bash
harn quickstart
source .env
```

For CI or scripts, use deterministic defaults:

```bash
harn quickstart --non-interactive --provider ollama --model llama3.2
```

You can also set an API key yourself and call a model directly:

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

For production callers, wrap with retry middleware from
`std/llm/handlers`:

```harn,ignore
import {default_llm_caller, with_retry} from "std/llm/handlers"

let caller = with_retry(default_llm_caller(), {max_attempts: 4})
let result = agent_loop(task, system, {llm_caller: caller, loop_until_done: true})
```

See [Composable callers and middleware](./stdlib/llm-handlers.md) for
fallback chains, shadowing, ensembles, and model-aware option packs.

## The REPL

Start an interactive session:

```bash
harn repl
```

The REPL evaluates expressions as you type and displays results
immediately. It keeps a persistent history in `~/.harn/repl_history` and
supports multi-line blocks until delimiters are balanced, which makes it useful
for experimenting with builtins and small snippets.

## Project setup

Scaffold a new project with `harn init` or pick a starter with `harn new`:

```bash
harn new my-agent --template agent
cd my-agent
harn quickstart --non-interactive
source .env
harn doctor --no-network
```

This creates a directory with `harn.toml` (project config) and starter files
for the selected template. Run it with:

```bash
harn run main.harn
```

For a streaming local chat loop, use the chat starter:

```bash
harn new my-chat --template chat
cd my-chat
harn run main.harn
```

The generated `harn.toml` points the `chat` model alias at Ollama by default.
Edit the alias or set `HARN_CHAT_MODEL` to use another configured provider.
See [LLM providers](./llm/providers.md) for provider setup.

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
- **[Workflow authoring quickstart](./workflow-authoring-quickstart.md)** -- Author, validate, preview, run, and
  supervise a portable workflow bundle without paid credentials
- **[Cookbook](./cookbook.md)** -- Practical recipes and patterns
