# Harn

**Harn is a programming language and runtime for building AI agents.**

- You write the parts that are yours: workflows, tools, policies, and prompts.
- **Harn** ([harnlang.com](https://harnlang.com/)) owns the plumbing every agent needs
so you don't have to roll your own or pull in a patchwork of libraries. These include
transcripts and session management, context assembly, retries, tool routing, provider
differences, persistence, replay, and evals.

```harn
tool search(pattern: string) -> string {
  description "Search the project"
  exec("rg", "--", pattern).stdout
}

const result = agent_loop(
  "Find the failing test and fix it.",
  "You are a senior engineer.",
  {loop_until_done: true, tools: search, max_iterations: 24}
)

log(result.status)        // "done"
log(result.visible_text)  // the agent's final answer
```

That loop is provider-agnostic, resumable, replayable, and produces a durable run record without any glue
code of your own.

Harn's CLI, runtime, and rich model/provider catalog handle footguns, quirks, and differences across LLM
APIs. The Harn toolchain helps you hit the ground running with local and cloud inference without
any of the pain of figuring out what context window works best, what fits on your machine, or how to deal with
an open-weight model whose native tool calling isn't great.

> **Pre-release.** Harn is pre-1.0; the language, standard library, and CLI can change between releases.
> Track changes in the [release notes](https://github.com/burin-labs/harn/releases) and
> [`CHANGELOG.md`](./CHANGELOG.md).

## Install

The one-line installer needs no Rust toolchain — it downloads the signed binary for your platform:

```bash
curl -fsSL https://harnlang.com/install.sh | sh
```

It detects your OS/CPU, downloads the matching binary from the latest
[GitHub release](https://github.com/burin-labs/harn/releases), verifies it against the release's
`SHA256SUMS`, and installs `harn`, `harn-dap`, and `harn-lsp` (macOS binaries are notarized). It honors
`$HARN_INSTALL_DIR` / `$XDG_BIN_DIR`, otherwise picks a `bin` dir already on your `PATH`, else creates
`$HOME/.harn/bin`. Upgrade later with `harn upgrade`.

With Cargo, or from source (both need **Rust 1.95+**):

```bash
cargo install harn-cli

git clone https://github.com/burin-labs/harn.git && cd harn
./scripts/dev_setup.sh && cargo install --path crates/harn-cli
```

### More install options

Shell completions:

```bash
harn completion bash > ~/.local/share/bash-completion/completions/harn
harn completion zsh  > ~/.zfunc/_harn      # ensure ~/.zfunc is on your fpath, then compinit
harn completion fish > ~/.config/fish/completions/harn.fish
```

Container image (multi-arch `linux/amd64` + `linux/arm64`, published to GHCR on release):

```bash
docker run -p 8080:8080 -v $PWD/triggers.toml:/etc/harn/triggers.toml \
  -e HARN_ORCHESTRATOR_API_KEYS=xxx ghcr.io/burin-labs/harn
```

The container defaults to `harn orchestrator serve` on `0.0.0.0:8080` with
`HARN_ORCHESTRATOR_MANIFEST=/etc/harn/triggers.toml`. Set `HARN_ORCHESTRATOR_API_KEYS` and
`HARN_ORCHESTRATOR_HMAC_SECRET` for authenticated `a2a-push` routes, and inject provider secrets via the
usual environment variables (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, or your `HARN_PROVIDER_*` /
`HARN_SECRET_*` values).

Cloud deploy templates for Render, Fly.io, and Railway live under `deploy/`. Generate a project-local
bundle and run the provider CLI:

```bash
harn orchestrator deploy --provider fly --manifest ./harn.toml --build
```

## Quick start

Run a bundled demo first — no API keys, no setup:

```bash
harn demo                 # menu of bundled scenarios
harn demo merge-captain   # persona-supervised PR triage
harn demo --list          # every scenario, with descriptions
```

Each demo runs in under 30 seconds against a checked-in LLM tape, so it behaves the same on your laptop with
zero credentials as it does in CI. Add `--live` to run against a real provider.

Then scaffold your own project:

```bash
harn new my-project --template agent
cd my-project
harn quickstart --non-interactive
source .env
harn doctor --no-network
harn run main.harn
harn test tests/
harn portal                # observability UI for your runs
```

The `agent` template lays out instructions, skills, tools, subagents, channels, sandbox, and schedules
wired through the Harn agent runtime.

## Writing agents

Tools are declared with typed parameters and an optional description:

```harn
tool read(path: string) -> string {
  description "Read a file"
  read_file(path)
}

tool test(filter: string) -> string {
  description "Run a targeted cargo test filter"
  const result = exec("cargo", "test", filter)
  result.stdout + result.stderr
}

const result = agent_loop(
  "Fix the failing test and verify the change.",
  "You are a senior engineer.",
  {loop_until_done: true, tools: read, max_iterations: 24}
)
```

For programmatic registration use `tool_define(...)`, which also preserves config keys such as `policy` for
capability enforcement.

Every turn's `llm_call(...)` can be wrapped with middleware to compose retry / fallback / shadow / budget /
cache behavior, without touching the loop:

```harn
import {default_llm_caller, with_retry, compose} from "std/llm/handlers"

const caller = compose([with_retry({max_attempts: 4, backoff: "exponential"})])(default_llm_caller())
agent_loop(task, system, {loop_until_done: true, llm_caller: caller})
```

See [the LLM handlers catalog](docs/src/stdlib/llm-handlers.md) for the full module set (handlers, ensemble,
refine, budget, defaults, safe, prompts, catalog).

## Why a runtime

Without a boundary like Harn, application code slowly absorbs concerns that aren't really the product:
provider-specific message parsing, transcript compaction, tool dispatch and retries, workflow branching and
repair loops, provenance and eval fixtures, host/editor queue semantics.

Harn keeps those in a typed runtime layer, so your app is left with the parts that are actually yours: the
capabilities you expose, your top-level policy ceilings, your workflow templates and product defaults, and
your UI/session integration.

## What's in the box

Capabilities are grouped below with pointers to the docs for depth. The full API is in the
[builtin reference](https://harnlang.com/builtins.html).

- **Agents & workflows** — `agent_loop(...)`, `sub_agent_run(...)`, and typed workflow graphs
  (`workflow_graph` / `workflow_execute`) with nodes, edges, validation, parallel map/join stages, retries,
  and resumable execution. Planner-oriented action graphs come from `import "std/agents"`.
- **Delegated workers** — `spawn_agent` / `send_input` / `resume_agent` / `wait_agent` / `close_agent` with
  child run lineage, persisted snapshots, per-worker execution scoping, and explicit continuation policy
  (`inherit` / `fork` / `reset` / `compact`) for what each worker carries.
- **Permissions & sandboxing** — capability ceilings that internal plans can narrow but never exceed,
  rule-based [approval policies](docs/src/builtins.md#declarative-tool-approval), argument-level tool
  constraints, and a per-platform [OS sandbox](docs/src/sandboxing.md) by default (Linux Landlock + seccomp,
  macOS sandbox-exec, Windows AppContainer).
- **Context & transcripts** — typed artifacts and resources as the real context boundary, budget-aware
  adaptive selection, automatic microcompaction and summarization, and structured transcript lifecycle
  (continue, fork, compact, render public-only or full history).
- **Providers & protocols** — normalized LLM output (`visible_text`, `tool_calls`, `blocks`, `provider`,
  `stop_reason`, …), composable LLM middleware, model-aware token counting, and remote MCP over stdio/HTTP
  with OAuth. Serve as an ACP backend, or run the A2A orchestrator.
- **Testing & evals** — typed host and LLM mocking (`host_mock`, `llm_mock`, `import "std/testing"`), plus
  portable eval packs and suites (`eval_pack`, `eval_suite_run`, `harn eval`, `harn test package --evals`)
  with rubric, threshold, timeout-ladder, and longitudinal-ledger grading as first-class runtime data.
- **Packages & extensibility** — a manifest-backed extension ABI (`[exports]`, `[[package.tools]]`, `[llm]`
  adapters in `harn.toml`), scaffolding (`harn tool new`, `harn package scaffold openapi`), local sibling
  linking (`harn add ../pkg`), and registry discovery (`harn package search` / `info` / `add @scope/name`).
- **Static preflight** — `harn check` resolves the full import graph, validates render/template paths and
  host-capability contracts, and rejects unknown symbols before the VM ever starts. The CLI, `harn run`, and
  the LSP share one module graph.
- **Standard library** — batteries such as `std/schema` (structured LLM I/O), `std/graphql`, `std/slug`,
  `std/vision` (deterministic OCR), `std/prompt_library`, `std/worktree`, `std/project`, and
  `std/personas/prelude` for reusable verifier/actor/approval control flow.

## Trust & provenance

Every run produces a durable record: persisted stage transcripts, artifacts, policy decisions, verification
outcomes, and delegated child lineage that you can inspect, replay, and evaluate after the fact.

Harn also emits portable `opentrustgraph/v0.1` trust records for autonomy decisions, approval gates, and
tier transitions. `v0.1` reserves three `metadata` keys — `effects_grant`, `effects_used`, and
`parent_record_id` — so a chain validator can prove a child agent's `effects_used` stayed inside its
parent's `effects_grant`. The public schema and fixtures live in
[`opentrustgraph-spec/`](./opentrustgraph-spec/).

### Trust boundary

Harn owns orchestration and provenance; hosts own concrete mutation UX.

- **Harn** owns workflow execution, transcript lifecycle, replay/eval, worker lineage, artifact provenance,
  and mutation-session audit metadata.
- **Hosts** own approvals, patch/apply UX, concrete file mutations, and editor undo/redo semantics.

For autonomous or background edits, the recommended default is worktree-backed execution plus explicit host
approval for destructive operations.

## Workflow example

```harn
const graph = workflow_graph({
  name: "review_and_repair",
  entry: "plan",
  nodes: {
    plan: {
      kind: "stage", mode: "llm", task_label: "Planning task",
      model_policy: {model_tier: "small"},
      context_policy: {include_kinds: ["summary", "resource"], max_tokens: 1200}
    },
    implement: {
      kind: "stage", mode: "agent", tools: coding_tools(),
      model_policy: {model_tier: "mid"}, retry_policy: {max_attempts: 2}
    },
    verify: {
      kind: "verify",
      verify: {command: "cargo test --workspace --quiet", expect_status: 0, assert_text: "test result: ok"}
    }
  },
  edges: [
    {from: "plan", to: "implement"},
    {from: "implement", to: "verify"},
    {from: "verify", to: "implement", branch: "failed"}
  ]
})

const run = workflow_execute("Refactor the parser error message and verify it.", graph, [], {max_steps: 8})
log(run.status)
log(run.path)
```

`harn runs view --json <run.path>` gives the stable `harn.run_view.v1` projection, including stage summaries.
A `verify` node can run an explicit command as shown, or use agent/LLM mode when verification should stay
provider-driven.

## Host integration

Run Harn as an ACP backend and inspect stable run/session views:

```bash
harn serve acp agent.harn
harn serve acp --transport websocket --bind 127.0.0.1:8789 agent.harn

harn portal
harn runs view --json .harn-runs/<run>.json
harn replay .harn-runs/<run>.json
harn eval .harn-runs/<run>.json
```

Queued human messages reach an in-flight agent with `session/inject` — `steer` injects after the current
tool boundary; `queue` defers until the agent yields control.

## Documentation

- [Docs book](https://harnlang.com/) · [CLI reference](https://harnlang.com/cli-reference.html) ·
  [Builtin reference](https://harnlang.com/builtins.html)
- [LLM quick reference](https://harnlang.com/docs/llm/harn-quickref.html)
- [Workflow runtime guide](https://harnlang.com/workflow-runtime.html) ·
  [LLM calls and agent loops](https://harnlang.com/llm-and-agents.html)
- [MCP, ACP, and A2A integration](https://harnlang.com/mcp-and-acp.html) ·
  [Protocol support matrix](https://harnlang.com/protocol-support.html)
- [Language spec](spec/HARN_SPEC.md) · [hosted version](https://harnlang.com/language-spec.html)

## Development

```bash
./scripts/dev_setup.sh   # git hooks, cargo-nextest, sccache, portal frontend, workspace check
make setup-rust          # focused Rust-only setup using a durable user-cache build root
make all                 # fmt + lint + test
make test                # Rust tests (uses cargo-nextest when available)
make conformance         # .harn conformance suite
make portal              # observability UI for runs under .harn-runs/
```

The workspace:

| Crate | Role |
| --- | --- |
| `harn-lexer` | scanner / tokenizer |
| `harn-parser` | parser, AST, type checker, diagnostics |
| `harn-vm` | compiler, interpreter, LLM/runtime/orchestration layer |
| `harn-fmt` | formatter |
| `harn-lint` | linter |
| `harn-cli` | CLI, ACP, A2A, conformance runner |
| `harn-lsp` | language server |
| `harn-dap` | debugger adapter |
| `tree-sitter-harn` | syntax grammar for editor integrations |

Maintainer release commands and gates live in
[the maintainer release workflow](docs/src/maintainer-release.md).
