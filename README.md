# Harn

[![CI](https://github.com/burin-labs/harn/actions/workflows/ci.yml/badge.svg)](https://github.com/burin-labs/harn/actions/workflows/ci.yml)

Harn is a programming language and runtime for orchestrating AI agents.
It sits between product code and provider/runtime code: products declare
workflows, policies, capabilities, and UI hooks, while Harn owns transcripts,
context assembly, retries, tool routing, persistence, replay, and provider
normalization.

Harn also emits portable `opentrustgraph/v0.1` trust records for autonomy
decisions, approval gates, and tier transitions. `v0.1` adds three
reserved `metadata` keys (`effects_grant`, `effects_used`,
`parent_record_id`) so chain validators can prove that a child agent's
`effects_used` stayed inside the parent's `effects_grant`. The public
schema and fixtures live in [`opentrustgraph-spec/`](./opentrustgraph-spec/).

## Install

One-line installer (recommended; no Rust toolchain required):

```bash
curl -fsSL https://harnlang.com/install.sh | sh
```

Detects OS/CPU, downloads the matching signed binary for the current
[GitHub release](https://github.com/burin-labs/harn/releases),
verifies it against the release's `SHA256SUMS` manifest, and installs
`harn`, `harn-dap`, and `harn-lsp`. It honors `$HARN_INSTALL_DIR` or
`$XDG_BIN_DIR` when set, otherwise uses `$HOME/bin` or `$HOME/.local/bin`
when one already exists and is on `PATH`, and falls back to creating
`$HOME/.harn/bin`. macOS binaries are notarized.
To upgrade later: `harn upgrade`.

With Cargo:

```bash
cargo install harn-cli
```

From source:

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
./scripts/dev_setup.sh
cargo install --path crates/harn-cli
```

Shell completions:

```bash
mkdir -p ~/.local/share/bash-completion/completions
harn completions bash > ~/.local/share/bash-completion/completions/harn

mkdir -p ~/.zfunc
harn completions zsh > ~/.zfunc/_harn
# Add to ~/.zshrc if needed: fpath=(~/.zfunc $fpath); autoload -Uz compinit; compinit

mkdir -p ~/.config/fish/completions
harn completions fish > ~/.config/fish/completions/harn.fish
```

Container image:

```bash
docker run -p 8080:8080 -v $PWD/triggers.toml:/etc/harn/triggers.toml -e HARN_ORCHESTRATOR_API_KEYS=xxx ghcr.io/burin-labs/harn
```

Release tags publish multi-arch `linux/amd64` and `linux/arm64` images to
GHCR. The container defaults to `harn orchestrator serve` with
`HARN_ORCHESTRATOR_MANIFEST=/etc/harn/triggers.toml` and
`HARN_ORCHESTRATOR_LISTEN=0.0.0.0:8080`; set
`HARN_ORCHESTRATOR_API_KEYS` and `HARN_ORCHESTRATOR_HMAC_SECRET` when
you expose authenticated `a2a-push` routes, and inject provider secrets
with the usual environment variables such as `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, or your deployment's `HARN_PROVIDER_*` /
`HARN_SECRET_*` values.

Cloud deploy templates for Render, Fly.io, and Railway live under
`deploy/`. To generate a project-local bundle and run the provider CLI:

```bash
harn orchestrator deploy --provider fly --manifest ./harn.toml --build
```

## Quick start

Run a bundled demo first. It needs no API keys or project setup:

```bash
harn demo                       # menu of bundled scenarios
harn demo merge-captain         # default scenario: persona-supervised PR triage
harn demo --list                # all scenarios with descriptions
harn demo provider-race --json  # machine-readable summary
```

Every demo runs in under 30 seconds against a checked-in LLM tape, so it
finishes the same way on a laptop with zero credentials as it does in
CI. Add `--live` to re-run against a configured provider.

Then scaffold a project of your own:

```bash
harn new my-project --template agent
cd my-project
harn quickstart --non-interactive
source .env
harn doctor --no-network
harn run main.harn
harn test tests/
harn portal
```

Remote MCP OAuth:

```bash
harn mcp redirect-uri
harn mcp login https://mcp.notion.com/mcp
```

`harn mcp login` prefers Harn's published CIMD client metadata document and
falls back to dynamic client registration when the authorization server does
not advertise CIMD support.

Simple LLM call:

```harn
let result = llm_call(
  "Explain quicksort in two sentences.",
  "You are a concise CS tutor."
)
log(result.visible_text)
```

Loop-until-done agent with tools:

```harn
tool read(path: string) -> string {
  description "Read a file"
  read_file(path)
}

tool search(pattern: string) -> string {
  description "Search project files"
  let result = exec("rg", "--", pattern)
  result.stdout + result.stderr
}

tool edit(path: string, content: string) -> string {
  description "Edit a file"
  write_file(path, content)
}

tool test(filter: string) -> string {
  description "Run a targeted cargo test filter"
  let result = exec("cargo", "test", filter)
  result.stdout + result.stderr
}

let result = agent_loop(
  "Fix the failing test and verify the change.",
  "You are a senior engineer.",
  {
    loop_until_done: true,
    tools: read,
    max_iterations: 24
  }
)

log(result.status)
log(result.visible_text)
```

The `tool` keyword declares tools with typed parameters and optional
descriptions. For programmatic tool registration, use `tool_define(...)`,
which also preserves extra config keys such as `policy` for capability
enforcement.

### Composable LLM middleware

`agent_loop` accepts an `llm_caller:` closure that owns each turn's
`llm_call(...)`. Wrap it with middleware from `std/llm/handlers` to
compose retry / fallback / shadow / budget / cache behavior:

```harn
import {default_llm_caller, with_retry, with_fallback, compose} from "std/llm/handlers"

let caller = compose([
  with_retry({max_attempts: 4, backoff: "exponential"}),
])(default_llm_caller())

agent_loop(task, system, {loop_until_done: true, llm_caller: caller})
```

See [docs/src/stdlib/llm-handlers.md](docs/src/stdlib/llm-handlers.md)
for the full module catalog (handlers, ensemble, refine, budget,
defaults, safe, prompts, catalog).

## Core capabilities

- Typed workflow graphs via `workflow_graph(...)` and `workflow_execute(...)`
  with explicit nodes, edges, validation, policy attachment, map/join style
  stages, and resumable execution.
- Planner-oriented action graphs via `import "std/agents"`:
  `action_graph(...)`, `action_graph_batches(...)`, `action_graph_flow(...)`,
  and `action_graph_run(...)` normalize planner schema variants into a shared
  executable schedule instead of leaving dependency repair and batch grouping
  to leaf pipelines.
- Persona orchestration primitives via `import "std/personas/prelude"`:
  verifier-then-actor gates, bounded loops, cheap-classifier escalation,
  circuit-broken parallel sweeps, audit receipt wrappers, and approval gates
  give durable personas reusable control flow without host-specific glue.
- Transparent profile bulletin proposals via
  `import "std/personas/bulletins"`: `bulletin_propose` builds typed
  `harn.profile_bulletin.v1` envelopes with stable id, scope, evidence,
  source, privacy, and TTL fields; `bulletin_emit` always writes proposals
  to `personas.bulletins.proposed`, and `bulletin_accept` / `bulletin_reject`
  / `bulletin_expire` / `bulletin_supersede` emit
  `harn.profile_bulletin_decision.v1` audit records so hosts (Burin local,
  Harn Cloud) can review persona context instead of accepting silent prompt
  mutation.
- Delegated worker lifecycle builtins via `spawn_agent(...)`, `send_input(...)`,
  `resume_agent(...)`, `wait_agent(...)`, `close_agent(...)`, and `list_agents()`,
  with child run lineage, persisted worker snapshots, and host-visible worker
  lifecycle events. Worker handles retain immutable original `request`
  metadata plus normalized `provenance` so parent orchestration can recover
  research questions, action items, workflow stages, and verification steps
  without positional rebinding. Agent loops also expose lifecycle tools for
  worker self-parking (`agent_await_resumption`) and opt-in parent-side
  subagent pause/resume control.
- Per-worker execution scoping on `spawn_agent(...)`: delegated workers inherit
  the current execution ceiling by default and can narrow it further with a
  `policy` dict or `tools: ["name", ...]` shorthand, with permission denials
  returned as structured tool results instead of opaque failures.
- `sub_agent_run(task, options?)` for isolated child agent loops that preserve a
  clean parent transcript while returning a typed summary envelope or a
  background worker handle.
- Explicit continuation policy for delegated workers: `carry.transcript_mode`
  (`inherit`, `fork`, `reset`, `compact`), artifact carryover, workflow resume
  control, and compact parent-facing `worker_result` artifacts.
- Runtime schema helpers for structured LLM I/O: `schema_check(...)`,
  `schema_parse(...)`, `schema_is(...)`, JSON Schema/OpenAPI conversion, and
  schema composition helpers, plus a lazy `std/schema` builder module for
  ergonomic schema authoring when imported.
- Provider-neutral GraphQL connector helpers via `import "std/graphql"`:
  request/envelope normalization, introspection and SDL fixture parsing,
  persisted-query metadata, cursor pagination helpers, auth headers, and
  generated-style operation wrapper source for GraphQL-first providers such as
  Linear.
- Prompt fragment reuse via `import "std/prompt_library"`: load TOML catalogs
  or front-matter `.harn.prompt` files, render cache-aware fragment payloads,
  and propose tenant-scoped k-means hotspots for repeated context prefixes.
- Deterministic vision OCR via `vision_ocr(...)` and `import "std/vision"`:
  image path / payload normalization, structured text output
  (`blocks`, `lines`, `tokens`), and event-log-backed OCR audit records for
  replayable agent/tool flows.
- Manifest-backed extension ABI: packages can publish stable module entry
  points via `[exports]`, declare custom tool and skill surfaces via
  `[[package.tools]]` and `[[package.skills]]`, and ship provider/alias
  adapters declaratively via `[llm]` in `harn.toml`, without editing core
  runtime registration code. `harn tool new <name>` scaffolds a Harn-native
  tool package with manifest metadata, tests, docs, and CI, while
  `harn package scaffold openapi` turns an OpenAPI spec into a focused
  generated SDK package with a regeneration script and package checks.
  Local sibling packages can be added with `harn add ../harn-openapi`;
  Harn derives the alias from the dependency's `harn.toml` and live-links
  directory path dependencies into `.harn/packages/` for fast multi-repo
  development. Registry-backed discovery is available through
  `harn package search`, `harn package info`, and
  `harn add @burin/<name>@<version>`, which resolve through the package
  index and then use the same git-backed install path as direct GitHub refs.
  Manifests can also pin direct git tags with `tag = "v1.2.3"` or resolve
  registry semver ranges with `version = "^1.2"`.
  `harn package list` and `harn package doctor` expose locked exports,
  permissions, host requirements, and materialized-package integrity for host
  UI and CI policy checks.
- Design-by-contract and project/runtime helpers: `require ...`,
  metadata/scanner runtime builtins, `import "std/project"` for
  freshness-aware metadata and scan state, and `import "std/runtime"` for
  generic runtime/process/interaction helpers inside Harn itself.
- Isolated execution substrate via directory-scoped command builtins
  (`exec_at`, `shell_at`) plus the `std/worktree` module for git worktree
  creation, status, diff, shell execution, and cleanup. Worker execution
  profiles can pin delegated runs to a cwd, env overlay, or managed
  worktree so background execution is reproducible instead of ambient-cwd
  dependent. Subprocesses spawned under an active capability ceiling
  run inside a per-platform OS sandbox by default: Linux Landlock +
  seccomp, macOS sandbox-exec, or Windows AppContainer + Job Object,
  selected via `CapabilityPolicy::sandbox_profile` and documented in
  [docs/src/sandboxing.md](docs/src/sandboxing.md). Pipelines that
  spawn untrusted code opt into `sandbox_profile: "os_hardened"` to
  make the OS confinement *required* (the spawn fails as
  `tool_rejected` if the platform mechanism is missing) instead of
  best-effort.
- Stronger preflight behavior via `harn check`: import graph resolution,
  literal template/render path validation, import symbol collision detection,
  and host capability contract validation all fail before runtime.
  `harn check` / `harn run` / the LSP share one recursive module graph that
  resolves every `import` (including `std/*` embeds) and rejects calls to names
  that are not builtins, local declarations, struct constructors, callable
  variables, or imported symbols, so stale or typo'd references surface before
  the VM starts. `render(...)` resolves relative to
  the module source tree (including inside imported modules) instead of the
  ambient process cwd. Literal delegated execution roots,
  `exec_at(...)` / `shell_at(...)` directories, and unknown
  `host_call("capability.operation", ...)` contracts are also checked before launch.
- Runtime-local typed host mocking for tests via `host_mock(...)`,
  `host_mock_clear()`, and `host_mock_calls()`, so `.harn` conformance and VM
  tests can exercise host-backed flows without requiring a live bridge host.
  `import "std/testing"` adds higher-level helpers such as
  `mock_host_result(...)`, `mock_host_error(...)`, and
  `assert_host_called(...)` for ordinary Harn tests.
- Configurable LLM mock responses via `llm_mock(...)`, `llm_mock_calls()`,
  and `llm_mock_clear()`: queue specific text, tool calls, or mixed
  responses for the mock provider. Supports FIFO queuing and glob-pattern
  matching against prompts.
- Eval suite manifests and portable eval packs via `eval_pack { ... }`,
  `eval_pack_manifest(...)`, resumable `eval_pack_run(...)`,
  `eval_ledger_*`, `eval_suite_manifest(...)`, `eval_suite_run(...)`,
  `persona_eval_ladder_run(...)`, `harn eval <manifest.json|harn.eval.toml>`,
  and `harn test package --evals`, so grouped replay, rubric, threshold,
  timeout-ladder, package-shipped connector evals, and longitudinal eval
  ledgers are first-class runtime data instead of external scripts.
- Typed artifacts and resources as the real context boundary. Context
  selection is artifact-aware, budget-aware, and policy-driven rather than
  raw prompt concatenation.
- Host-facing artifact helpers for workspace files, snapshots, editor
  selections, command/test/verification outputs, and diff/review decisions,
  so product code can pass structured state into Harn without rebuilding
  artifact taxonomy or provenance conventions.
- Durable run records with persisted stage transcripts, artifacts, policy
  decisions, verification outcomes, delegated child lineage, and
  inspection/replay/eval entrypoints including recursive run-tree loading.
- Provider-normalized LLM output with `visible_text`, `private_reasoning`,
  `thinking_summary`, `tool_calls`, `blocks`, `provider`, `stop_reason`, and
  transcript events.
- Structured transcript lifecycle support: continue, fork, compact,
  summarize, render public-only output, or render full execution history.
- Workflow meta-editing builtins such as `workflow.inspect`, clone/insert/
  replace/rewire operations, per-node model/context/transcript policy edits,
  diff, validate, and commit-style validation.
- Capability ceiling enforcement for workflows and sub-orchestration:
  internal plans may narrow capabilities but cannot exceed the host ceiling.
- ACP pending user-message injects for agent execution: accept with a stable
  `messageId`, optionally replace or revoke while pending, steer after the
  current operation, or queue until the agent yields back to the human.
- ACP pending reminder controls for operator UIs: inspect the bridge queue and
  revoke queued `session/remind` reminders before a checkpoint drains them.
- Remote MCP over stdio and HTTP, including OAuth metadata discovery, stored
  bearer tokens for standalone CLI use, and automatic token reuse for HTTP MCP
  servers declared in `harn.toml`.
- Runtime semantic cleanup for older surfaces: repeated `catch e { ... }`
  bindings work within the same enclosing block, and float division keeps
  IEEE `NaN`/`Infinity` behavior instead of raising runtime errors.
- Formatter width handling wraps oversized comma-separated forms
  consistently across calls, list literals, dict literals, enum payloads, and
  struct-style construction instead of leaving long single-line output intact.
- Tool lifecycle hooks via `register_tool_hook(...)`: pre-execution deny/modify
  and post-execution result interception for agent tool calls, with glob-pattern
  matching on tool names.
- Automatic transcript compaction in agent loops: microcompaction snips oversized
  tool outputs, auto-compaction triggers at configurable token thresholds, and
  `compact_strategy` supports default LLM summarization, truncate fallback, or
  custom Harn closure-based compaction. Host/user compaction instructions flow
  through the typed `CompactionPolicy` lane so `/compact <instructions>` style
  commands can reuse runtime audit/events without bespoke prompt wiring. The
  same pipeline is exposed directly as `transcript_auto_compact(...)`.
- Daemon agent mode (`daemon: true`): agents stay alive waiting for
  host-injected messages instead of terminating on text-only responses, with
  adaptive idle backoff, persisted snapshots, timer/file-watch wakes, and
  explicit bridge wake/resume signaling.
- Per-agent capability policies with argument-level constraints: `agent_loop`
  accepts a `policy` dict to scope tool permissions, including `tool_arg_constraints`
  for pattern-matching on tool arguments.
- Rule-based approval policies: `approval_policy.rules` expresses allow/ask/deny
  over tool names/kinds, side-effect levels, declared paths, command identity,
  URLs/domains/methods, MCP identity, agent/persona/mode, and repeat counts,
  with deny-by-default sensitive path guards and replayable policy-decision
  receipts in permission events and host approval prompts.
- Dynamic per-agent permissions: `agent_loop`, `sub_agent_run`, and
  `spawn_agent` accept `permissions` with `allow` / `deny` tool rules, VM
  predicates over the tool args, and `on_escalation` callbacks that can grant a
  denied call once or for the session. Permission decisions emit
  `PermissionGrant`, `PermissionDeny`, and `PermissionEscalation` transcript
  events.
- Generic call-site type checking is stricter: `where`-clause interface
  violations are errors, repeated generic parameters must bind to one concrete
  type, and container bindings like `list<T>` propagate their element type.
- Workflow map stages can execute in parallel with `"all"`, `"first"`, or
  `"quorum"` join strategies plus `max_concurrent` throttling.
- LSP completions surface inferred shape fields, struct members, and enum
  payload fields on dot access instead of defaulting to dict methods.
- Adaptive context assembly with deduplication and microcompaction via
  `select_artifacts_adaptive(...)`, plus `estimate_tokens(...)` and
  `microcompact(...)` utility builtins.
- Model-aware token counting via `tiktoken_count_tokens(...)` and
  `std/llm/budget`, with exact tiktoken counts for known OpenAI models and
  labeled approximations for Claude/Gemini model families.
- Host-aware static preflight: `harn check` can load host-specific capability
  schemas and alternate bundle roots from `harn.toml` or CLI flags so host
  adapters and bundled template layouts validate cleanly.
- Mutation-session audit metadata for workflows, delegated workers, and bridge
  tool gates so hosts can group write-capable operations under one trust
  boundary without forcing one edit-application UX.
- String method aliases for case normalization: `.lower()`, `.upper()`,
  `.to_lower()`, and `.to_upper()`.

## Trust boundary

Harn owns orchestration and provenance. Hosts own concrete mutation UX.

- Harn owns workflow execution, transcript lifecycle, replay/eval, worker
  lineage, artifact provenance, and mutation-session audit metadata.
- Hosts own approvals, patch/apply UX, concrete file mutations, and editor
  undo/redo semantics.

For autonomous or background edits, the recommended default is worktree-backed
execution plus explicit host approval for destructive operations.

## Release workflow

Maintainer release commands and gates live in
[Maintainer release workflow](docs/src/maintainer-release.md).

## Local development

For a local contributor setup:

```bash
./scripts/dev_setup.sh
make all
make portal
```

`dev_setup.sh` configures git hooks, installs `cargo-nextest` and `sccache`,
installs repo-local Node tooling including the portal frontend, builds
`crates/harn-cli/portal-dist`, enables the sccache rustc wrapper, and runs a
workspace `cargo check`. When `CODEX_WORKTREE_PATH` is set, it also writes a
per-worktree temp `target-dir` into `.cargo/config.toml` so parallel Codex
worktrees do not fight over one shared Cargo target. Heavy setup phases are
fingerprinted under `.codex/dev-setup/`, so rerunning the script in an already
prepared checkout is normally a fast no-op; set `HARN_DEV_SETUP_FORCE=1` to
refresh all phases. The tracked Codex local
environment at `.codex/environments/environment.toml` calls the same script for
new app-managed worktrees. Claude Code project settings use a `SessionStart`
hook to run `scripts/claude-dev-setup-once.sh`, which delegates to this script
once per dependency fingerprint and writes logs under `.claude/dev-setup/`.
`make portal` launches the built-in observability UI
for persisted runs under `.harn-runs/`.

The repo-root portal scripts (`npm run portal:lint`, `portal:test`,
`portal:build`, and `portal:dev`) self-bootstrap
`crates/harn-cli/portal/node_modules` from the checked-in lockfile when those
dependencies are missing, and the git hooks call the same bootstrap path before
portal lint runs.

## Why this matters

Without a runtime boundary like Harn, application code often accumulates:

- provider-specific message/response parsing
- transcript compaction and summarization logic
- tool dispatch and retry behavior
- workflow branching and repair loops
- provenance, replay, and eval fixtures
- host/editor queue semantics

Harn keeps those concerns in a typed runtime layer so a host app can focus on:

- capabilities it wants to expose
- top-level policy ceilings
- workflow templates and product defaults
- UI/session integration

## Workflow runtime example

```harn
let graph = workflow_graph({
  name: "review_and_repair",
  entry: "plan",
  nodes: {
    plan: {
      kind: "stage",
      mode: "llm",
      task_label: "Planning task",
      model_policy: {model_tier: "small"},
      context_policy: {include_kinds: ["summary", "resource"], max_tokens: 1200}
    },
    implement: {
      kind: "stage",
      mode: "agent",
      tools: coding_tools(),
      model_policy: {model_tier: "mid"},
      retry_policy: {max_attempts: 2}
    },
    verify: {
      kind: "verify",
      verify: {
        command: "cargo test --workspace --quiet",
        expect_status: 0,
        assert_text: "test result: ok"
      }
    }
  },
  edges: [
    {from: "plan", to: "implement"},
    {from: "implement", to: "verify"},
    {from: "verify", to: "implement", branch: "failed"}
  ]
})

let artifacts = [
  artifact({
    kind: "resource",
    title: "Editor selection",
    text: read_file("src/lib.rs"),
    source: "workspace"
  })
]

let run = workflow_execute(
  "Refactor the parser error message and verify it.",
  graph,
  artifacts,
  {max_steps: 8}
)

log(run.status)
log(run.path)
```

Use `harn runs view --json <path>` on `run.path` for the stable
`harn.run_view.v1` projection, including stage summaries. `verify` nodes can
either run an explicit command as shown above or use an agent/LLM mode when
verification should stay provider-driven.

## Transcript and artifact model

`llm_call(...)` and `agent_loop(...)` return a canonical schema that
separates human-visible output from internal execution state:

- `visible_text`: safe assistant-visible text
- `private_reasoning`: provider reasoning metadata when available
- `thinking_summary`: provider-supplied reasoning summary when available
- `tool_calls`: normalized tool intent
- `blocks`: canonical structured blocks across providers
- `provider`: normalized provider identity
- `transcript`: persisted transcript state with `messages` and `events`

Artifact records are durable typed objects with provenance:

```harn
let note = artifact({
  kind: "analysis_note",
  title: "Parser regression risk",
  text: "The lexer span mapping affects diagnostics and tree-sitter tests.",
  source: "review",
  relevance: 0.9,
  metadata: {owner: "runtime"}
})

let focused = artifact_select([note], {
  include_kinds: ["analysis_note"],
  max_tokens: 200
})
```

## Host integration

Run Harn as an ACP backend:

```bash
harn serve acp agent.harn
harn serve acp --transport websocket --bind 127.0.0.1:8789 agent.harn
harn serve acp --api-key "$HARN_ACP_KEY" agent.harn
HARN_PROFILE_JSON=/tmp/acp.ndjson harn serve acp agent.harn
```

Inspect stable run/session views:

```bash
harn portal
harn runs view --json .harn-runs/<run>.json
harn runs view --json --session .harn-runs/
harn replay .harn-runs/<run>.json
harn eval .harn-runs/<run>.json
```

Queued human messages can be delivered to an in-flight agent with
`session/inject`:

- `steer`: inject after the current tool/operation boundary
- `queue`: defer until the agent yields control

## Documentation

- [Docs book](https://harnlang.com/)
- [LLM quick reference](https://harnlang.com/docs/llm/harn-quickref.html)
- [Workflow runtime guide](https://harnlang.com/workflow-runtime.html)
- [LLM calls and agent loops](https://harnlang.com/llm-and-agents.html)
- [Protocol support matrix](https://harnlang.com/protocol-support.html)
- [MCP, ACP, and A2A integration](https://harnlang.com/mcp-and-acp.html)
- [CLI reference](https://harnlang.com/cli-reference.html)
- [Builtin reference](https://harnlang.com/builtins.html)
- [Language spec](spec/HARN_SPEC.md)
- [Hosted language spec](https://harnlang.com/language-spec.html)

## Development

```bash
make fmt
make lint
make test           # default Rust test path; uses cargo-nextest when available
make test-cargo     # force plain cargo test --workspace
make test-fast      # compatibility alias for make test
make conformance
harn test conformance --timing
harn test conformance tests/worktree_runtime.harn
make all
```

The workspace includes:

- `harn-lexer`: scanner/tokenizer
- `harn-parser`: parser, AST, type checker, diagnostics
- `harn-vm`: compiler, interpreter, LLM/runtime/orchestration layer
- `harn-fmt`: formatter
- `harn-lint`: linter
- `harn-cli`: CLI, ACP, A2A, conformance runner
- `harn-lsp`: language server
- `harn-dap`: debugger adapter
- `tree-sitter-harn`: syntax grammar for editor integrations
