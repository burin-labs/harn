# CLI reference

All commands available in the `harn` CLI.

## harn run

Execute a `.harn` file.

```bash
harn run <file.harn>
harn run --trace <file.harn>
harn run -e 'println("hello")'
harn run --deny shell,exec <file.harn>
harn run --allow read_file,write_file <file.harn>
```

| Flag | Description |
|---|---|
| `--trace` | Print LLM trace summary after execution |
| `-e <code>` | Evaluate inline code instead of a file |
| `--deny <builtins>` | Deny specific builtins (comma-separated) |
| `--allow <builtins>` | Allow only specific builtins (comma-separated) |

You can also run a file directly without the `run` subcommand:

```bash
harn main.harn
```

Before starting the VM, `harn run <file>` builds the cross-module
graph for the entry file. When all imports resolve, unknown call
targets produce a static error and the VM is never started — the same
`call target ... is not defined or imported` message you see from
`harn check`. The inline `-e <code>` form has no importing file and
therefore skips the cross-module check.

## harn playground

Run a pipeline against a Harn-native host module for fast local iteration.

```bash
harn playground --host host.harn --script pipeline.harn --task "Explain this repo"
harn playground --watch --task "Refine the prompt"
harn playground --llm ollama:qwen2.5-coder:latest --task "Use a local model"
```

| Flag | Description |
|---|---|
| `--host <file>` | Host module exporting the functions the script expects (default: `host.harn`) |
| `--script <file>` | Pipeline entrypoint to execute (default: `pipeline.harn`) |
| `--task <text>` | Task string exposed as `HARN_TASK` during the run |
| `--llm <provider:model>` | Override the provider/model selection for this invocation |
| `--llm-mock <path>` | Replay LLM responses from a JSONL fixture file instead of calling the provider |
| `--llm-mock-record <path>` | Record executed LLM responses into a JSONL fixture file |
| `--watch` | Re-run when the host module or script changes |

`harn playground` type-checks the host module, merges its exported function
names into the script's static call-target validation, then executes the script
with an in-process host adapter. Missing host functions fail with a pointed
error naming the function and caller location.

## harn test

Run tests.

```bash
harn test conformance                  # run conformance test suite
harn test conformance tests/language/arithmetic.harn # run one conformance file
harn test conformance tests/stdlib/     # run a conformance subtree
harn test tests/                       # run user tests in directory
harn test tests/ --filter "auth*"      # filter by pattern
harn test tests/ --parallel            # run tests concurrently
harn test tests/ --watch               # re-run on file changes
harn test conformance --verbose        # show per-test timing
harn test conformance --timing         # show timing summary without verbose failures
harn test tests/ --record              # record LLM fixtures
harn test tests/ --replay              # replay LLM fixtures
```

| Flag | Description |
|---|---|
| `--filter <pattern>` | Only run tests matching pattern |
| `--parallel` | Run tests concurrently |
| `--watch` | Re-run tests on file changes |
| `--verbose` / `-v` | Show per-test timing and detailed failures |
| `--timing` | Show per-test timing plus summary statistics |
| `--junit <path>` | Write JUnit XML report |
| `--timeout <ms>` | Per-test timeout in milliseconds (default: 30000) |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay recorded LLM responses |

When no path is given, `harn test` auto-discovers a `tests/` directory
in the current folder. Conformance targets must resolve to a file or directory
inside `conformance/`; the CLI now errors instead of silently falling back to
the full suite when a requested target is missing.

## harn repl

Start an interactive REPL with syntax highlighting, multiline editing, live
builtin completion, and persistent history in `~/.harn/repl_history`.

```bash
harn repl
```

The REPL keeps incomplete blocks open until braces, brackets, parentheses, and
quoted strings are balanced, so you can paste or type multi-line pipelines and
control-flow blocks directly.

## harn bench

Benchmark a `.harn` file over repeated runs.

```bash
harn bench main.harn
harn bench main.harn --iterations 25
```

`harn bench` parses and compiles the file once, executes it with a fresh VM for
each iteration, and reports wall time plus aggregated LLM token, call, and cost
metrics.

## harn viz

Render a `.harn` file as a Mermaid flowchart.

```bash
harn viz main.harn
harn viz main.harn --output docs/graph.mmd
```

`harn viz` parses the file, walks the AST, and emits a Mermaid `flowchart TD`
graph showing pipelines, functions, branches, loops, and other workflow-shaped
control-flow nodes.

## harn fmt

Format `.harn` source files. Accepts files or directories.

```bash
harn fmt main.harn
harn fmt src/
harn fmt --check main.harn            # check mode (no changes, exit 1 if unformatted)
harn fmt --line-width 80 main.harn    # custom line width
```

| Flag | Description |
|---|---|
| `--check` | Check mode: exit 1 if any file would be reformatted, make no changes |
| `--line-width <N>` | Maximum line width before wrapping (default: 100) |

The formatter enforces a **100-character line width** by default (overridable with `--line-width`). When a line exceeds
this limit the formatter wraps it automatically:

`harn fmt` also normalizes optional semicolon-separated statements back to the canonical
newline-separated style. Semicolons are accepted as input syntax in statement-list
contexts, but they are not preserved in formatter output.

- **Comma-separated forms** — function call arguments, function declaration
  parameters, list literals, dict literals, struct construction fields,
  enum constructor payloads, selective import names, interface method
  parameters, and enum variant fields all wrap with one item per line and
  trailing commas.
- **Binary operator chains** — long expressions like `a + b + c + d` break
  before the operator. Operators that the parser cannot resume across a bare
  newline (`-`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `in`, `not in`, `??`)
  get an automatic backslash continuation (`\`); other operators (`+`, `*`,
  `/`, `%`, `||`, `&&`, `|>`) break without one.
- **Operator precedence parentheses** — the formatter inserts parentheses
  to preserve semantics when the AST drops them (e.g. `a * (b + c)` stays
  parenthesised) and for clarity when mixing `&&` / `||`
  (e.g. `a && b || c` becomes `(a && b) || c`).

## harn lint

Lint one or more `.harn` files or directories for common issues (unused
variables, unused functions, unreachable code, empty blocks, missing
`/** */` HarnDoc on public functions, etc.).

```bash
harn lint main.harn
harn lint src/ tests/
```

Pass `--fix` to automatically apply safe fixes (e.g., `var` → `let` for
never-reassigned bindings, boolean comparison simplification, unused import
removal, and string interpolation conversion):

```bash
harn lint --fix main.harn
```

## harn check

Type-check one or more `.harn` files or directories and run preflight
validation without executing them. The preflight pass resolves imports, checks
literal `render(...)` / `render_prompt(...)` targets, detects import symbol collisions across
modules, validates `host_call("capability.operation", ...)` capability
contracts, and flags missing template resources, execution directories, and worker repos that would
otherwise fail only at runtime. Source-aware lint rules run as part of
`check`, including the `missing-harndoc` warning for undocumented `pub fn`
APIs.

`check` builds a cross-module graph from each entry file and follows
`import` statements recursively. When every import in a file resolves,
the typechecker knows the exact set of names that module brings into
scope and will emit a hard error for any call target that is neither a
builtin, a local declaration, a struct constructor, a callable
variable, nor an imported symbol:

```text
error: call target `helpr` is not defined or imported
```

This catches typos and stale imports before the VM runs. If any import
in the file is unresolved, the stricter check is turned off for that
file so one broken import does not avalanche into spurious errors — the
unresolved import itself still fails at runtime.

```bash
harn check main.harn
harn check src/ tests/
harn check --host-capabilities host-capabilities.json main.harn
harn check --bundle-root .bundle main.harn
harn check --workspace
harn check --preflight warning src/
```

| Flag | Description |
|---|---|
| `--host-capabilities <file>` | Load a host capability manifest for preflight validation. Supports plain `{capability: [ops...]}` objects, nested `{capabilities: ...}` wrappers, and per-op metadata dictionaries. Overrides `[check].host_capabilities_path` in `harn.toml`. |
| `--bundle-root <dir>` | Validate `render(...)`, `render_prompt(...)`, and template paths against an alternate bundled layout root |
| `--workspace` | Walk every path listed in `[workspace].pipelines` of the nearest `harn.toml`. Positional targets remain additive. |
| `--preflight <severity>` | Override preflight diagnostic severity: `error` (default, fails the check), `warning` (reports but does not fail), or `off` (suppresses all preflight diagnostics). Overrides `[check].preflight_severity`. |
| `--strict-types` | Flag unvalidated boundary-API values used in field access. |

### harn.toml — `[check]` and `[workspace]` sections

`harn check` walks upward from the target file (stopping at the first `.git`
directory) to find the nearest `harn.toml`. The following keys are honored:

```toml
[check]
# Load an external capability manifest. Path is resolved relative to
# harn.toml. Accepts JSON or TOML with the namespaced shape
# { workspace = [...], process = [...], project = [...], ... }.
host_capabilities_path = "./schemas/host-capabilities.json"

# Or declare inline:
[check.host_capabilities]
project = ["ensure_enriched", "enrich"]
workspace = ["read_text", "write_text"]

[check]
# Downgrade preflight errors to warnings (or suppress entirely with "off").
# Keeps type diagnostics visible while an external capability schema is
# still catching up to a host's live surface.
preflight_severity = "warning"

# Suppress preflight diagnostics for specific capabilities/operations.
# Entries match either an exact "capability.operation" pair, a
# "capability.*" wildcard, a bare "capability" name, or a blanket "*".
preflight_allow = ["mystery.*", "runtime.task"]

[workspace]
# Directories or files checked by `harn check --workspace`. Paths are
# resolved relative to harn.toml.
pipelines = ["Sources/BurinCore/Resources/pipelines", "scripts"]
```

Preflight diagnostics are reported under the `preflight` category so they
can be distinguished from type-checker errors in IDE output streams and
CI log filters.

## harn contracts

Export machine-readable contracts for hosts, release tooling, and embedded
bundles.

```bash
harn contracts builtins
harn contracts host-capabilities --host-capabilities host-capabilities.json
harn contracts bundle main.harn --verify
harn contracts bundle src/ --bundle-root .bundle --host-capabilities host-capabilities.json
```

### harn contracts builtins

Print the parser/runtime builtin registry as JSON, including return-type hints
and alignment status.

### harn contracts host-capabilities

Print the effective host-capability manifest used by preflight validation after
merging the built-in defaults with any external manifest file.

### harn contracts bundle

Print a bundle manifest for one or more `.harn` targets. The manifest includes:

- explicit `entry_modules`, `import_modules`, and `module_dependencies` edges
- explicit `prompt_assets` and `template_assets` slices, plus a full `assets`
  table resolved through the same source-relative rules as `render(...)`
- required host capabilities discovered from literal `host_call(...)` sites
- literal execution directories and worker worktree repos
- a `summary` block with stable counts for packagers and release tooling

Use `--verify` to run normal Harn preflight validation before emitting the
bundle manifest and return a non-zero exit code if the selected targets are not
bundle-safe.

## harn init

Scaffold a new project with `harn.toml` and `main.harn`.

```bash
harn init              # create in current directory
harn init my-project   # create in a new directory
harn init --template eval
```

## harn new

Scaffold a new project from a starter template. Supported templates are
`basic`, `agent`, `mcp-server`, and `eval`.

```bash
harn new my-agent --template agent
harn new local-mcp --template mcp-server
harn new eval-suite --template eval
```

`harn init` and `harn new` share the same scaffolding engine. Use `init` for
the default quick-start flow and `new` when you want the template choice to be
explicit.

## harn doctor

Inspect the local environment and report the current Harn setup,
including the resolved secret-provider chain and keyring health.

```bash
harn doctor
harn doctor --no-network
```

## harn connect linear

Register a Linear webhook through the GraphQL `webhookCreate` mutation using
the Linear triggers declared in the nearest `harn.toml`.

The command derives `resourceTypes` from `[[triggers]]` entries with
`provider = "linear"` and requires either `--team-id` or
`--all-public-teams`.

```bash
harn connect linear \
  --url https://example.com/hooks/linear \
  --team-id 72b2a2dc-6f4f-4423-9d34-24b5bd10634a \
  --access-token-secret linear/access-token

harn connect linear \
  --url https://example.com/hooks/linear \
  --all-public-teams \
  --api-key-secret linear/api-key \
  --json
```

Auth options:

- `--access-token` or `--access-token-secret`
- `--api-key` or `--api-key-secret`

Use `--config <path>` to point at an explicit manifest instead of discovering
the nearest one from the current working directory.

## harn watch

Watch a file for changes and re-run it automatically.

```bash
harn watch main.harn
harn watch --deny shell main.harn
```

## harn acp

Start an ACP (Agent Client Protocol) server on stdio.

```bash
harn acp                    # bridge mode, no pipeline
harn acp pipeline.harn      # execute a pipeline per prompt
```

See [MCP and ACP Integration](./mcp-and-acp.md) for protocol details.

## harn portal

Launch the local Harn observability portal for persisted runs.

```bash
harn portal
harn portal --dir runs/archive
harn portal --host 0.0.0.0 --port 4900
harn portal --open false
```

See [Harn Portal](./portal.md) for the full guide.

## harn runs

Inspect persisted workflow run records.

```bash
harn runs inspect .harn-runs/<run>.json
harn runs inspect .harn-runs/<run>.json --compare baseline.json
```

## harn replay

Replay a persisted workflow run record from saved output.

```bash
harn replay .harn-runs/<run>.json
```

## harn eval

Evaluate a persisted workflow run record as a regression fixture.

```bash
harn eval .harn-runs/<run>.json
harn eval .harn-runs/<run>.json --compare baseline.json
harn eval .harn-runs/
harn eval evals/regression.json
```

`harn eval` accepts three inputs:

- a single run record JSON file
- a directory of run record JSON files
- an eval suite manifest JSON file with grouped cases and optional baseline comparisons

## harn orchestrator

Long-running manifest-driven orchestrator for trigger ingestion and
connector activation. See [Orchestrator](./orchestrator.md) for full
detail.

```bash
# Start the orchestrator against a manifest. Binds HTTP(S) for
# webhook/a2a-push triggers, activates connectors, writes a state
# snapshot, drains cleanly on SIGTERM/SIGINT, reloads on SIGHUP.
harn orchestrator serve \
  --config harn.toml \
  --state-dir .harn/orchestrator \
  --bind 0.0.0.0:8080 \
  --role single-tenant

# Inspect the running orchestrator state snapshot + bindings.
harn orchestrator inspect --state-dir .harn/orchestrator

# Inject a synthetic TriggerEvent to exercise a specific binding.
harn orchestrator fire <trigger-id> --payload event.json

# Replay a historical event through the dispatcher.
harn orchestrator replay <event-id>

# Inspect the dead-letter queue.
harn orchestrator dlq list
harn orchestrator dlq --replay <event-id>

# Inspect the pending-queue head.
harn orchestrator queue

# List worker queues + stranded dispatcher envelopes explicitly.
harn orchestrator queue --config harn.toml --state-dir ./.harn/orchestrator ls

# Drain one worker queue with a local consumer manifest.
harn orchestrator queue --config harn.toml --state-dir ./.harn/orchestrator drain <queue>

# Drop ready jobs from a worker queue.
harn orchestrator queue --config harn.toml --state-dir ./.harn/orchestrator purge <queue> --confirm
```

`harn orchestrator inspect/fire/replay/dlq/queue` are offline
operations — they read the state snapshot + event log directly. To
operate against a live `harn orchestrator serve`, use the same state
directory. Environment variables `HARN_ORCHESTRATOR_MANIFEST`,
`HARN_ORCHESTRATOR_LISTEN`, `HARN_ORCHESTRATOR_STATE_DIR`,
`HARN_ORCHESTRATOR_API_KEYS`, and `HARN_ORCHESTRATOR_HMAC_SECRET`
configure the serve entry point for container deployments.

## harn trigger replay

Replay a persisted `TriggerEvent` from a standalone EventLog snapshot
through the dispatcher (no orchestrator needed).

```bash
# Replay an event, re-dispatch against the live binding.
harn trigger replay <event-id>

# Compare replay result vs. original (structured drift JSON).
harn trigger replay <event-id> --diff

# Replay against a historical binding version by timestamp.
harn trigger replay <event-id> --as-of 2026-04-19T12:00:00Z

# Preview a filtered bulk replay without dispatching anything.
harn trigger replay --where "event.payload.tenant == 'acme' AND attempt.status == 'failed'" --dry-run

# Replay matching records with progress output and throttling.
harn trigger replay --where "attempt.failed_at > '2026-04-18'" --progress --rate-limit 4
```

Sets `HARN_REPLAY=1` during dispatch so nondeterminism in handlers
can fall back to recorded values when the handler cooperates.

Bulk replay selection uses a Harn expression over event-log records with
top-level `event`, `binding`, `attempt`, `outcome`, and `audit` objects.
The CLI accepts SQL-ish convenience syntax for filters: single-quoted
strings plus `AND`/`OR`/`NOT` normalize into the underlying Harn
expression evaluator before dispatch.

`--dry-run` returns the matching records without replaying them.
`--progress` streams per-item progress to stderr, and `--rate-limit`
caps bulk execution throughput in operations per second.

Every bulk replay appends an audit envelope to
`trigger.operations.audit` describing who ran it, when, the normalized
filter, and the affected records.

## harn trigger cancel

Request cancellation for pending or in-flight non-replay trigger
dispatches recorded in the EventLog snapshot.

```bash
# Cancel a single event/binding lineage if it is still active.
harn trigger cancel <event-id>

# Preview which active runs match a filter.
harn trigger cancel --where "attempt.handler == 'handlers::risky'" --dry-run

# Request cancellation for matching runs with progress output.
harn trigger cancel --where "event.payload.tenant == 'acme'" --progress --rate-limit 4
```

Cancellation writes durable control records to `trigger.cancel.requests`
and the dispatcher polls that topic while dispatches are queued,
sleeping between retries, or running local handlers. Terminal runs are
reported as `not_cancellable` and are left unchanged.

## harn trust query

Query trust-graph records from the workspace event log.

```bash
# List all trust records for one agent.
harn trust query --agent github-triage-bot

# Filter by action, tier, and outcome, then emit JSON.
harn trust query \
  --agent github-triage-bot \
  --action github.issue.opened \
  --tier act-auto \
  --outcome success \
  --json

# Aggregate per-agent stats.
harn trust query --summary
```

Supported filters:

- `--agent`
- `--action`
- `--since`
- `--until`
- `--tier`
- `--outcome`
- `--json`
- `--summary`

`--summary` groups records by agent and reports success rate, mean recorded
cost, tier distribution, and outcome distribution.

## harn trust promote

Manually promote an agent to a higher autonomy tier. This appends a
`trust.promote` control record to the trust graph.

```bash
harn trust promote github-triage-bot --to act-auto
harn trust promote reviewer-bot --to act-with-approval
```

## harn trust demote

Manually demote an agent and record the reason in trust-graph metadata.

```bash
harn trust demote github-triage-bot --to shadow --reason "unexpected mutation"
harn trust demote deploy-bot --to suggest --reason "needs tighter review gate"
```

## harn serve

Start an A2A (Agent-to-Agent) HTTP server.

```bash
harn serve agent.harn               # default port 8080
harn serve --port 3000 agent.harn   # custom port
```

See [MCP and ACP Integration](./mcp-and-acp.md) for protocol details.

## harn mcp-serve

Serve a Harn pipeline as an MCP server over stdio.

```bash
harn mcp-serve agent.harn
```

See [MCP and ACP Integration](./mcp-and-acp.md) for details on defining
tools, resources, and prompts.

## harn mcp

Manage standalone OAuth state for remote HTTP MCP servers.

```bash
harn mcp redirect-uri
harn mcp login notion
harn mcp login https://mcp.notion.com/mcp
harn mcp login my-server --url https://example.com/mcp --client-id <id> --client-secret <secret>
harn mcp status notion
harn mcp logout notion
```

`harn mcp login` resolves the server from the nearest `harn.toml` when you pass
an MCP server name, or uses the explicit URL when you pass `--url` or a raw
`https://...` target. The CLI:

- discovers OAuth protected resource and authorization server metadata
- prefers pre-registered `client_id` / `client_secret` values when supplied
- falls back to dynamic client registration when supported by the server
- stores tokens in the local OS keychain and refreshes them automatically

Relevant flags:

| Flag | Description |
|---|---|
| `--url <url>` | Explicit MCP server URL when logging in/out by a custom name |
| `--client-id <id>` | Use a pre-registered client ID instead of dynamic registration |
| `--client-secret <secret>` | Optional client secret for `client_secret_post` / `client_secret_basic` servers |
| `--scope <scopes>` | Override or provide requested OAuth scopes |
| `--redirect-uri <uri>` | Override the default loopback redirect URI (default shown by `harn mcp redirect-uri`) |

Security guidance:

- prefer the narrowest scopes the remote MCP server supports
- treat configured `client_secret` values as secrets
- review remote MCP capabilities before using them in autonomous workflows

## Release gate

For repo maintainers, the deterministic full-release path is:

```bash
./scripts/release_ship.sh --bump patch
```

This runs audit → dry-run publish → bump → commit → tag → push → `cargo publish`
→ GitHub release in that order. Pushing happens before `cargo publish` so
downstream consumers (GitHub release binary workflows, `burin-code`'s
`fetch-harn`) start in parallel with crates.io.

For piecewise work, the docs audit, verification gate, bump flow, and publish
sequence are exposed individually:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```

## harn add

Add a dependency to `harn.toml`.

```bash
harn add https://github.com/user/my-lib@v1.2.3
harn add https://github.com/user/my-lib --alias my-lib
harn add my-lib --git https://github.com/user/my-lib --rev v1.2.3   # legacy form
```

## harn install

Install dependencies declared in `harn.toml`, writing or reusing
`harn.lock` and materializing `.harn/packages/`.

```bash
harn install
harn install --frozen
harn install --refetch my-lib
```

## harn lock

Resolve dependencies from `harn.toml` and write `harn.lock` without
materializing packages.

```bash
harn lock
```

## harn update

Refresh one dependency (or all of them) and update `harn.lock`.

```bash
harn update my-lib
harn update --all
```

## harn remove

Remove one dependency from `harn.toml`, `harn.lock`, and
`.harn/packages/`.

```bash
harn remove my-lib
```

## harn version

Show version information.

```bash
harn version
```
