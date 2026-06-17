# Getting started

This page gets you from zero to running your first Harn program.

## Installation

### One-line installer (recommended)

```bash
curl -fsSL https://harnlang.com/install.sh | sh
```

Detects your OS and CPU, downloads the matching signed binary from the
latest GitHub release, verifies its SHA256 against the release manifest,
and installs `harn`, `harn-dap`, and `harn-lsp` into the first writable
target among `$HARN_INSTALL_DIR`, `$XDG_BIN_DIR`, `$HOME/bin`,
`$HOME/.local/bin`, or `$HOME/.harn/bin`. macOS binaries are notarized,
so Gatekeeper validates them on first launch with no extra prompts.

To pin a specific release, pass `HARN_VERSION`:

```bash
curl -fsSL https://harnlang.com/install.sh | HARN_VERSION=v0.8.32 sh
```

To upgrade later, run `harn upgrade` — it reuses the same release
artifacts and SHA256SUMS manifest to atomically replace the running
binary.

### From crates.io

If you already have a Rust toolchain:

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

## Prerequisites for building from source

- **[Rust](https://rustup.rs/)** 1.70 or later -- install with
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Git**

The one-line installer and `cargo install harn-cli` do not require a
Rust toolchain on the user's machine.

### See Harn in action in 30 seconds

Before configuring anything, run a bundled offline demo. No API keys,
no project setup, no network — every demo replays from a JSONL tape
embedded in the binary:

```bash
harn demo                  # menu of bundled scenarios
harn demo merge-captain    # persona-supervised PR triage
harn demo --list           # one-line summary of every scenario
```

See [`harn demo` in the CLI reference](cli-reference.md#harn-demo) for
the full surface and `--live` opt-in.

### Run this first

`harn doctor` is the one-command environment readiness check. It probes the
toolchain, optional dev tools, portal dependencies, platform capabilities,
provider credentials, and protocol artifact freshness, then prints a
red/yellow/green summary with the exact fix command for anything that needs
attention.

```bash
harn doctor                # local checks; skips remote provider probes by default
harn doctor --check-providers  # actively probe configured providers
harn doctor --json         # machine-readable output for preflight automation
```

The JSON output is versioned (`schema_version`) and stable across patch
releases — Burin Code and Harn Cloud read the `summary.blocked_flows` array to
decide whether a host can build, test, release, run scripts, or work on the
portal.

### Optional shell completions

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
fn main(harness: Harness) {
  harness.stdio.println("Hello, world!")
}
```

Run it:

```bash
harn run hello.harn
```

That's it. For capability-aware scripts, `fn main(harness: Harness)` is the
canonical entrypoint: the runtime passes in the script's `Harness` handle and
you route side effects through `harness.*`.

Harn still supports top-level code without boilerplate. For tiny one-off
snippets, the runtime can wrap top-level statements as an **implicit pipeline**,
but the explicit `main(harness: Harness)` form is the recommended starting
point once a script needs stdio, clock, filesystem, env, random, or network
access.

## Adding a pipeline

For larger workflow-style programs, organize code into named pipelines. The
runtime executes the `default` pipeline (or the first one declared):

```harn
pipeline default(task) {
  let name = "Harn"
  log("Hello from ${name}!")
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
  "You are a computer science tutor.",
  {provider: "anthropic", model: "claude-sonnet-4-6"}
)
log(response)
```

The third argument is the model route. You can omit it after `harn quickstart`
writes defaults, or when environment variables select a provider. With no
explicit options, Harn checks `HARN_LLM_PROVIDER`, then `HARN_LLM_MODEL`, then
the default provider from `HARN_DEFAULT_PROVIDER` or provider config. The
built-in default is Anthropic when its key is available; otherwise Harn can fall
back to keyless Ollama/local routes. See [LLM providers](./llm/providers.md)
for the full resolution order, provider table, and `providers.toml` format.

No imports, no SDK initialization, no response parsing.

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
harn doctor
```

This creates a directory with `harn.toml` (project config) and starter files
for the selected template. The `agent` template uses an opinionated `agent/`
layout: `agent/instructions.md`, `agent/app.harn`, `agent/skills/`, and
convention folders for tools, subagents, channels, sandbox policy, and durable
schedules. Run it with:

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

Use a remote MCP server when a Harn program needs tools hosted outside your
machine. Notion is the common example: the server runs on Notion's side, Harn
stores your OAuth token locally, and your `.harn` code calls the server through
the normal MCP builtins.

```bash
harn mcp login notion
harn mcp status notion
```

`harn mcp login notion` opens the browser, completes OAuth with PKCE, and
stores the token in the local OS keychain. `harn mcp redirect-uri` just prints
the default callback URI (`http://127.0.0.1:9783/oauth/callback`) for servers
that ask you to pre-register one; you usually do not need it for built-in
presets such as Notion.

Then declare the server in `harn.toml`:

```toml
[[mcp]]
name = "notion"
transport = "http"
url = "https://mcp.notion.com/mcp"
```

And call it from a program:

```harn,ignore
fn main(harness: Harness) {
  let pages = mcp_call(mcp.notion, "search", {query: "release notes"})
  harness.stdio.println(json_stringify_pretty(pages))
}
```

For the complete client/server surface, see
[MCP, ACP, and A2A integration](./mcp-and-acp.md).

## Next steps

- **[Why Harn?](./why-harn.md)** -- What problems Harn solves
- **[Language basics](./language-basics.md)** -- Syntax, types, control flow
- **[LLM calls and agent loops](./llm-and-agents.md)** -- Calling models and building agents
- **[Workflow authoring quickstart](./workflow-authoring-quickstart.md)** -- Author, validate, preview, run, and
  supervise a portable workflow bundle without paid credentials
- **[Cookbook](./cookbook.md)** -- Practical recipes and patterns
