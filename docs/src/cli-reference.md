# CLI reference

All commands available in the `harn` CLI.

## harn run

Execute a `.harn` file.

```bash
harn run <file.harn>
harn run --trace <file.harn>
harn run --profile --profile-json profile.json <file.harn>
harn run -e 'println("hello")'
harn run --deny shell,exec <file.harn>
harn run --allow read_file,write_file <file.harn>
harn run --yes <file.harn>
harn run --explain-cost <file.harn>
harn run --attest <file.harn>
harn run --attest --receipt-out receipt.json <file.harn>
```

| Flag | Description |
|---|---|
| `--trace` | Print LLM trace summary after execution. Can also be set with `HARN_TRACE=1` |
| `--profile` | Print a categorical timing breakdown after execution. Can also be set with `HARN_PROFILE=1` |
| `--profile-json <path>` | Write the categorical timing breakdown as JSON. Can also be set with `HARN_PROFILE_JSON=<path>` |
| `--explain-cost` | Print static LLM token/cost estimates without executing the script |
| `-e <code>` | Evaluate inline code instead of a file |
| `--deny <builtins>` | Deny specific builtins (comma-separated) |
| `--allow <builtins>` | Allow only specific builtins (comma-separated) |
| `--yes` | Accept first-run provider setup prompts, including local Ollama config seeding |
| `--attest` | Emit a signed provenance receipt after execution |
| `--receipt-out <path>` | Write the receipt to a specific JSON path |
| `--attest-agent <id>` | Agent id used to load or create the Ed25519 signing key |

You can also run a file directly without the `run` subcommand:

```bash
harn main.harn
```

Before starting the VM, `harn run <file>` builds the cross-module
graph for the entry file. When all imports resolve, unknown call
targets produce a static error and the VM is never started — the same
`call target ... is not defined or imported` message you see from
`harn check`.

The inline `-e <code>` form is wrapped in `pipeline main(task) { ... }`
and run as a temp file in the current directory, so:

- Leading `import "..."` (and `pub import { ... } from "..."`) lines
  are hoisted out of the wrapper. They must come first; imports that
  appear after another statement are not lifted.
- Relative imports resolve against the working directory:
  `harn run -e $'import "./lib"\nprintln(answer())'` looks for
  `./lib.harn` next to where you invoked `harn`.
- Stdlib imports (`import "std/..."`) work the same as in a file.
- A read-only working directory falls back to the system temp dir;
  pure-expression `-e` still works there but relative imports won't
  resolve.

`--explain-cost` stops after the static pass. It scans direct `llm_call(...)`
and `agent_loop(...)` callsites, estimates statically known prompt/system input
tokens, applies literal `agent_loop` iteration caps, and reports unresolved
dynamic model or prompt expressions without executing pipeline code.

When `--attest` is present, Harn records run start/finish events in an
EventLog, stamps each record with `prev_hash` and `record_hash` provenance
headers, signs the receipt with an Ed25519 key loaded through the configured
secret provider chain, and writes the receipt under `.harn/receipts/` unless
`--receipt-out` is set.

## harn verify

Verify a signed Harn provenance receipt.

```bash
harn verify .harn/receipts/<receipt>.json
harn verify receipt.json --json
```

Verification recomputes the EventLog record chain, the receipt event root, the
receipt hash, and the Ed25519 signature. It exits non-zero if any receipt event
or signature material has been changed.

## harn session

Export, validate, import, and check canonical session bundles.

```bash
harn session export .harn-runs/<run>/run.json --out run.bundle.json
harn session export .harn-runs/<run>/run.json --local --out run.local.bundle.json
harn session export .harn-runs/<run>/run.json --replay-only --out run.replay.bundle.json
harn session validate run.bundle.json --json
harn session import run.bundle.json --out imported-run.json
harn session schema --check
```

| Subcommand | Description |
|---|---|
| `export <run-record>` | Writes a `harn_session_bundle` envelope. Default output is sanitized; `--local` preserves local-only content; `--replay-only` withholds prompt/tool payload fields. |
| `validate <bundle>` | Validates required fields, schema version, bundle type, and high-confidence secret markers without writing a run record. |
| `import <bundle>` | Validates the bundle and materializes a local run record from `replay.run_record` or replay metadata. |
| `schema` | Prints, writes, or checks `spec/schemas/session-bundle.v1.schema.json`. |

`export --include-attachments` copies artifact payloads into the top-level
`attachments` array. By default attachment payloads are omitted for share
safety. `import` and `validate` accept
`--allow-unsafe-secret-markers` only for trusted local bundles.

## harn completions

Print shell completion scripts.

```bash
harn completions bash
harn completions zsh
harn completions fish
```

Generated completions include subcommands plus static candidates for known
provider and model values.

## harn config

Inspect, validate, and emit schemas for layered runtime configuration.

```bash
harn config inspect
harn config inspect --explain
harn config inspect --config ./harn.config.toml --managed ./org-policy.toml --explain
harn config validate ./harn.config.toml
harn config schema --output docs/src/schemas/harn-config.schema.json
```

| Command | Description |
|---|---|
| `inspect` | Print the redacted merged runtime config |
| `inspect --explain` | Include per-field provenance, shadowed candidates, and managed policy lock status |
| `validate` | Validate local, project, or managed overlays against the typed config shape |
| `schema` | Print the editor JSON Schema for `harn.config.toml` |

See [Layered runtime configuration](./configuration.md) for precedence, file
locations, environment override names, and managed policy examples.

## harn playground

Run a pipeline against a Harn-native host module for fast local iteration.
`harn try` is an alias for the same command.

```bash
harn playground --host host.harn --script pipeline.harn --task "Explain this repo"
harn playground --watch --task "Refine the prompt"
harn playground --llm ollama:qwen2.5-coder:latest --task "Use a local model"
harn try --yes --task "Use local Ollama"
```

| Flag | Description |
|---|---|
| `--host <file>` | Host module exporting the functions the script expects (default: `host.harn`) |
| `--script <file>` | Pipeline entrypoint to execute (default: `pipeline.harn`) |
| `--task <text>` | Task string exposed as `HARN_TASK` during the run |
| `--llm <provider:model>` | Override the provider/model selection for this invocation |
| `--llm-mock <path>` | Replay LLM responses from a JSONL fixture file instead of calling the provider |
| `--llm-mock-record <path>` | Record executed LLM responses into a JSONL fixture file |
| `--yes` | Accept first-run provider setup prompts, including local Ollama config seeding |
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
harn test agents-conformance --target http://localhost:8080 --api-key "$KEY"
```

| Flag | Description |
|---|---|
| `--filter <pattern>` | Only run tests matching pattern |
| `--target <url>` | Harness base URL for `harn test agents-conformance` |
| `--api-key <key>` | Bearer API key for `harn test agents-conformance` |
| `--category <name>` | Agents conformance category to run; repeatable or comma-separated |
| `--json` / `--json-out <path>` | Emit or write the agents conformance leaderboard-shaped JSON report |
| `--workspace-id <id>` / `--session-id <id>` | Reuse existing Harness resources for agents conformance setup |
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

Benchmark a `.harn` file over repeated runs, or score deterministic replay
fixtures.

```bash
harn bench main.harn
harn bench main.harn --iterations 25
harn bench main.harn --iterations 25 --profile-json bench.json
harn bench replay --json --output replay-benchmark.json
harn bench replay conformance/replay-oracle/fixtures --filter approval --json
```

`harn bench` parses and compiles the file once, executes it with a fresh VM for
each iteration, and reports wall time plus aggregated LLM token, call, and cost
metrics. The wall-time rollup includes min, mean, p50, p95, max, stddev, and
total. `--profile` prints the aggregate categorical timing rollup; `--profile-json`
writes a JSON report with `iterations[]`, `mean_ms`, `p50_ms`, `p95_ms`,
`stddev_ms`, and `rollup`. `HARN_PROFILE=1` and `HARN_PROFILE_JSON=<path>` work
as environment aliases.

`harn bench replay` reads `benchmarks/replay/suite.json` by default. It emits
schema `harn.replay_benchmark.report.v1`, including Harn Cloud ingest metadata,
fixture receipts, replay-fidelity scores, permission-preservation scores,
tool-call drift counts, transcript drift counts, observed interaction totals,
and first-divergence triage data. Use `--output <path>` to write the JSON report
and `--json` to print it to stdout. The command exits non-zero when any fixture
fails its expected replay result.

External trace pairs can be adapted with the documented OpenCode-inspired JSONL
adapter:

```bash
harn bench replay \
  --adapter opencode-jsonl \
  --external-first benchmarks/replay/adapters/opencode/first.jsonl \
  --external-second benchmarks/replay/adapters/opencode/second.jsonl \
  --external-name opencode-permission-run \
  --json
```

## harn viz

Render a `.harn` file as a Mermaid flowchart.

```bash
harn viz main.harn
harn viz main.harn --output docs/graph.mmd
```

`harn viz` parses the file, walks the AST, and emits a Mermaid `flowchart TD`
graph showing pipelines, functions, branches, loops, and other workflow-shaped
control-flow nodes.

## harn flow

Inspect and operate the Flow shipping substrate.

```bash
harn flow replay-audit --store .harn/flow.sqlite --predicate-root . --touched-dir crates/harn-vm
harn flow replay-audit --since 2026-04-26 --fail-on-drift --json
harn flow ship watch --store .harn/flow.sqlite --predicate-root . \
  --touched-dir crates/harn-vm --mock-pr-out .harn/flow/mock-pr.json --json
harn flow archivist scan . --out .harn/flow/archivist-proposals.json --json
```

`replay-audit` compares predicate hashes pinned in derived slices with the
current `invariants.harn` predicate set. Drift is advisory unless
`--fail-on-drift` is present.

`ship watch` is the Phase 0 Ship Captain shadow-mode surface. It groups stored
atoms into intent summaries, derives a candidate slice, discovers applicable
`invariants.harn` predicates for the touched directories, persists a local
shipping receipt, and can write a mock PR receipt without touching a remote
GitHub repository.

`archivist scan` emits review-ready predicate proposal metadata from the repo's
stack hints and existing Flow predicates. It is propose-only; it does not edit
`invariants.harn`.

## harn persona

List, inspect, and control durable agent persona manifests from `harn.toml`.

```bash
harn persona list
harn persona list --json
harn persona inspect merge_captain
harn persona inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml status merge_captain --json
harn persona --manifest examples/personas/harn.toml tick merge_captain --json
harn persona --manifest examples/personas/harn.toml trigger merge_captain \
  --provider github --kind pull_request \
  --metadata repository=burin-labs/harn --metadata number=462 --json
harn persona --manifest examples/personas/harn.toml pause merge_captain
harn persona --manifest examples/personas/harn.toml resume merge_captain
harn persona --manifest examples/personas/harn.toml disable merge_captain
harn persona --manifest examples/personas/harn.toml supervision tail --json
harn persona --manifest examples/personas/harn.toml supervision tail \
  --persona merge_captain --since-event-id 42 --follow --limit 200 --json
```

| Flag | Description |
|---|---|
| `--manifest <path>` | Use an explicit `harn.toml` path or directory containing one |
| `--state-dir <dir>` | Store persona runtime events under a durable EventLog base directory, default `.harn/personas` |
| `--json` | Emit stable JSON for list, inspect, status, controls, trigger, tick, and budget receipts. `supervision tail` always emits NDJSON frames and accepts `--json` for host symmetry |
| `--persona <name>` | Filter `supervision tail` to one persona; omitted streams every persona in the local state directory |
| `--since-event-id <N>` | Replay `supervision tail` frames with `event_id > N` |
| `--follow` | Keep `supervision tail` open and stream new appended frames |
| `--limit <N>` | Cap emitted `supervision tail` frames |

`harn persona` validates the manifest before printing. It rejects missing entry
workflows, unknown capabilities, invalid budget fields, invalid schedules, and
handoffs that point at undeclared personas.

Runtime commands append event-sourced lifecycle records to
`persona.runtime.events`. `pause` queues matching trigger events,
`resume` drains queued events once under a lease, and `disable` records later
events as dead-lettered. `tick`, `trigger`, and `spend` enforce per-persona
daily, hourly, run, and token budgets before recording expensive-work receipts.
`supervision tail` projects those records into hosted-compatible
`persona/update` NDJSON frames with `event_id`, `persona_id`, `persona_kind`,
optional `persona_version`, `actor`, `update_kind`, `occurred_at`, and
`payload`.

## harn fmt

Format `.harn` source files. Accepts files or directories.

```bash
harn fmt main.harn
harn fmt src/
harn fmt --check main.harn            # check mode (no changes, exit 1 if unformatted)
harn fmt --line-width 80 main.harn    # custom line width
harn fmt --separator-width 60 main.harn # fixed section-header separator width
```

| Flag | Description |
|---|---|
| `--check` | Check mode: exit 1 if any file would be reformatted, make no changes |
| `--line-width <N>` | Maximum line width before wrapping (default: 100) |
| `--separator-width <N>` | Fixed total width for normalized `// ----` section-header separators (default: `line-width` minus indent) |

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
  newline (`-`, `in`, `not in`) get an automatic backslash continuation (`\`);
  other operators (`+`, `*`, `/`, `%`, `||`, `&&`, `|>`, `==`, `!=`, `<`,
  `>`, `<=`, `>=`, `??`) break without one.
- **Operator precedence parentheses** — the formatter inserts parentheses
  to preserve semantics when the AST drops them (e.g. `a * (b + c)` stays
  parenthesised) and for clarity when mixing `&&` / `||`
  (e.g. `a && b || c` becomes `(a && b) || c`).

## harn lint

Lint one or more `.harn` files or directories for common issues (unused
variables, unused functions, dead code after terminating statements,
pointless comparisons, redundant clones, empty blocks, missing `/** */`
HarnDoc on public functions, etc.).

```bash
harn lint main.harn
harn lint src/ tests/
```

Pass `--fix` to automatically apply safe fixes (e.g., `var` → `let` for
never-reassigned bindings, boolean comparison simplification, unused import
removal, string interpolation conversion, `let`-then-`return` simplification,
and removing redundant `clone()`/`to_string(...)`/`to_int(...)`/`to_list(...)`
wrappers or unnecessary parentheses around single values):

```bash
harn lint --fix main.harn
```

The Harn LSP also exposes these autofixes as quick-fix code actions
(Cmd+./Ctrl+. in most editors) and as a bulk `source.fixAll.harn`
action that VS Code can run on save:

```jsonc
"[harn]": {
  "editor.codeActionsOnSave": { "source.fixAll.harn": "always" }
}
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
harn check --invariants main.harn
harn check --workspace
harn check --preflight warning src/
```

| Flag | Description |
|---|---|
| `--host-capabilities <file>` | Load a host capability manifest for preflight validation. Supports plain `{capability: [ops...]}` objects, nested `{capabilities: ...}` wrappers, and per-op metadata dictionaries. Overrides `[check].host_capabilities_path` in `harn.toml`. |
| `--bundle-root <dir>` | Validate `render(...)`, `render_prompt(...)`, and template paths against an alternate bundled layout root |
| `--invariants` | Evaluate `@invariant(...)` annotations on functions, tools, and pipelines. Violations fail the check and are reported as `invariant[<name>]` diagnostics with concrete source spans. |
| `--workspace` | Walk every path listed in `[workspace].pipelines` of the nearest `harn.toml`. Positional targets remain additive. |
| `--preflight <severity>` | Override preflight diagnostic severity: `error` (default, fails the check), `warning` (reports but does not fail), or `off` (suppresses all preflight diagnostics). Overrides `[check].preflight_severity`. |
| `--strict-types` | Flag unvalidated boundary-API values used in field access. |

`--invariants` includes the capability-policy lattice:

```harn,ignore
@invariant("capability.policy",
  allow: "fs.write,process.exec,mcp.connector,llm.model",
  workspace: "src/**",
  require_approval: "fs.write",
  require_command_policy: "process.exec",
  require_egress_policy: "mcp.connector",
  require_budget: "llm.model",
)
fn release_worker(client) {
  let _approval = request_approval("edit", {capabilities_requested: ["fs.write"]})
  write_file("src/release.md", "ready")
  with_command_policy({deny: ["rm"]}, { -> exec("git status") })
  egress_policy({default: "deny", allow: ["api.github.com"]})
  mcp_call(client, "github.search", {query: "harn"})
  llm_call("summarize", nil, {budget: {max_output_tokens: 128}})
}
```

Canonical capability names are `fs.write`, `process.exec`,
`network.access`, `mcp.connector`, `llm.model`, `worker.dispatch`,
`human.approval`, and `autonomy.policy`.

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
pipelines = ["pipelines", "scripts"]
```

Preflight diagnostics are reported under the `preflight` category so they
can be distinguished from type-checker errors in IDE output streams and
CI log filters.

## harn explain

Explain the control-flow path behind one invariant violation for a single
handler. This is the companion to `harn check --invariants`: `check`
answers whether a handler violates its declared contract, and `explain`
shows the path that makes the violation reachable.

```bash
harn explain --invariant fs.writes write_patch main.harn
harn explain --invariant approval.reachability deploy_agent agent.harn
harn explain --invariant budget.remaining spend_budget budget.harn
```

`harn explain` loads the file, rebuilds the same handler IR used by
`check`, and prints:

- the invariant name and handler
- the violation message plus any help text
- a numbered CFG path showing the source locations traversed to reach the
  violating call or assignment

If the handler does not exist or does not declare the requested
`@invariant(...)`, the command exits nonzero with a direct error message.

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
`basic`, `agent`, `chat`, `mcp-server`, `eval`, `pipeline-lab`, `package`, and
`connector`.

```bash
harn new my-agent --template agent
harn new my-chat --template chat
harn new local-mcp --template mcp-server
harn new eval-suite --template eval
harn new package my-lib
```

`harn init` and `harn new` share the same scaffolding engine. Use `init` for
the default quick-start flow and `new` when you want the template choice to be
explicit.

## harn tool

Scaffold Harn-native custom tool packages.

```bash
harn tool new acme-echo
harn tool new acme-echo --dir packages/acme-echo
harn tool new acme-echo --description "Echo text for tests."
```

The generated package includes `[[package.tools]]` metadata, a stable
`tools` export, package-local dispatch tests, API docs, and CI commands for
`harn test`, `harn package check`, `harn package docs --check`, and
`harn package pack --dry-run`.

## harn skill

Scaffold or inspect one custom skill. `harn skill new` is the singular alias
for `harn skills new`.

```bash
harn skill new review-helper
```

## harn demo

Run a bundled, fully-offline scenario that demonstrates Harn end-to-end
without any API keys, project setup, or network access. Designed for
the cold-start "what does Harn actually do?" moment.

```bash
harn demo                         # menu of bundled scenarios (default: merge-captain)
harn demo merge-captain           # persona-supervised PR triage with structured receipts
harn demo review-captain          # HITL clarifying-question loop on a 5-file diff
harn demo provider-race           # latency-aware provider race with cost attribution
harn demo --list                  # one-line summary of every scenario
harn demo merge-captain --json    # machine-readable summary of the run
harn demo merge-captain --live    # re-run against the configured LLM provider
harn demo merge-captain --no-record  # skip writing a `.harn-runs/demo-*` record
```

Each scenario embeds a `.harn` script and a JSONL `--llm-mock` tape into
the `harn` binary, so every demo is reproducible across machines and
finishes in well under a second on the bundled tape. Successful runs
write a `run.json` under `.harn-runs/demo-<scenario>-<timestamp>/`
that the portal can inspect.

When `--live` is set the demo skips the bundled tape and routes through
your configured provider. If the failure looks like a missing/invalid
provider key the demo prints a hint to re-run without `--live` (or to
run `harn quickstart` to wire a provider).

## harn quickstart

Inspect local provider readiness and write starter LLM configuration.

```bash
harn quickstart
harn quickstart --non-interactive
harn quickstart --non-interactive --provider ollama --model llama3.2
```

Quickstart reports existing provider credentials, Ollama reachability, free disk
space, and GPU detection. It creates or updates:

- `~/.config/harn/providers.toml` or `HARN_PROVIDERS_CONFIG` when set
- `harn.toml` in the current directory
- `.env` in the current directory

Run `source .env` before invoking Harn if you want the shell to use the selected
provider/model defaults.

## harn doctor

One-command readiness check for new contributors and host integrators. The
command surfaces actionable, secret-free output for both humans and machine
consumers (Burin Code preflight, Harn Cloud onboarding).

```bash
harn doctor                # default: full check including provider /models probes
harn doctor --no-network   # skip remote checks (CI-friendly)
harn doctor --json         # versioned machine-readable output
```

Each check reports a red/yellow/green status (`fail` / `warn` / `ok`, plus
`skip` when a check is not applicable) along with:

- A single-line `detail`.
- An optional `fix_command` that the user can copy-paste.
- An optional `docs_url` pointing at relevant documentation.
- A `blocks` array naming workflows that fail when this check fails. Stable
  values are `build`, `test`, `release`, `publish`, `portal`, `scripting`,
  and `editor`.

The command exits non-zero when at least one check is `fail`.

### What it checks

- **Toolchain** — `rustc`, `cargo` (FAIL when missing).
- **Optional dev tools** — `cargo-nextest`, `sccache`, `actionlint` (WARN
  when missing; each has a documented fallback).
- **Protocol artifacts** — when run inside the harn repo, compares the
  pinned `HARN_PROTOCOL_ARTIFACT_VERSION` in
  `spec/protocol-artifacts/harn-protocol.ts` against the binary's own
  version and FAILs on drift.
- **Portal frontend** — `node`, `npm`, and the portal's `node_modules`
  directory when run inside the repo.
- **Platform capabilities** — `notify` file-watcher backend availability and
  the system browser opener used by OAuth and `harn portal`.
- **Provider configuration** — `HARN_PROVIDERS_CONFIG`, `HARN_LLM_PROVIDER`,
  the resolved secret-provider chain, and per-provider env vars (printed by
  name only — never by value).
- **Manifest** — nearest `harn.toml`, declared MCP servers, and registered
  triggers.
- **Runtime state** — event log backend, metadata cache, loaded skills,
  Ollama presence and pulled models, hardware snapshot.
- **Provider connectivity** (`--no-network` to skip) — for OpenAI-compatible
  local providers, `/v1/models` probes parse the listing and report missing
  configured models instead of only checking HTTP reachability.

### JSON schema

The JSON document is versioned via the top-level `schema_version` string;
patch releases never break the documented field shapes.

```json
{
  "schema_version": "1",
  "harn_version": "0.8.4",
  "providers_config_path": "...",
  "model_defaults": { ... },
  "checks": [
    {
      "id": "rustc",
      "label": "rustc",
      "status": "ok",
      "detail": "rustc 1.84.0 (...)",
      "fix_command": "https://rustup.rs",
      "docs_url": "https://www.rust-lang.org/tools/install",
      "blocks": ["build", "test", "release", "publish"]
    }
  ],
  "summary": {
    "ok": 18, "warn": 2, "fail": 0, "skip": 1,
    "blocked_flows": []
  },
  "hardware": { "ram_gb": 64, "gpu": "Apple Silicon (MPS available)", "free_disk_gb": 240 },
  "next_step": "..."
}
```

Secrets never appear in any output. Credential checks list the env var
**names** they looked for, not the values.

## harn provider-ready

Probe a configured provider's `/models` endpoint and optionally require a
specific model alias or provider-native model id.

```bash
harn provider-ready mlx --model mlx-qwen36-27b
harn provider-ready mlx --base-url http://127.0.0.1:8002 --json
```

The command exits non-zero for unreachable servers, bad HTTP status,
unparsable model listings, and missing models. It does not run local launcher
scripts; host applications that auto-start local servers should report launch
failures themselves and then call this probe again.

## harn provider-probe

Snapshot a provider's readiness and local loaded-model state as JSON. For
Ollama, the output also includes `/api/ps` memory/context details. When a
model is supplied, local runtime profile metadata is included for eval
pipelines.

```bash
harn provider-probe ollama --model qwen3.6-coding
harn provider-probe mlx --model mlx-qwen36-27b --base-url http://127.0.0.1:8002
```

## harn provider-tool-probe

Run a harmless one-tool conformance probe. The command asks the model to call
`echo_marker({value})`, tests streaming and non-streaming modes by default, and
emits JSON with native/text/disabled fallback classification.

```bash
harn provider-tool-probe ollama --model qwen3.6-coding
harn provider-tool-probe llamacpp --model local-qwen3.6 --mode non-streaming
```

Use `--response-fixture` to classify a saved provider response without making a
network request. `harn local switch` can consume the JSON with `--probe-result`.

## harn local profile

Explain the local runtime risk profile for a model/provider route:

```bash
harn local profile qwen3.6-coding --provider ollama
harn local profile ollama-gemma4 --json
```

Statuses are `preferred`, `experimental`, `vision_only_experimental`,
`quarantined`, or `unknown`. `harn local switch` refuses experimental and
quarantined profiles unless the required probes pass or `--force` is supplied.

## harn models test

Round-trip a small prompt through one resolved model and report model id,
provider, latency, first streamed delta timing, token usage, and estimated
cost.

```bash
harn models test gpt-4o-mini --prompt "Reply with pong."
harn models test qwen3:30b --provider ollama --json
```

`--provider` bypasses provider inference for the model selector. The command
uses the configured provider client path, so it also respects provider
credentials, base URL overrides, and `HARN_LLM_CALLS_DISABLED`.

## harn model-info

Print resolved model metadata as JSON. For Ollama models, `--verify` probes
`/api/tags` and checks the selected tag. `--warm` implies `--verify` and sends
an empty `/api/generate` request to preload the matched tag.

```bash
harn model-info llama3.2:latest
harn model-info --verify llama3.2
harn model-info --warm --keep-alive 30m llama3.2
```

Ollama readiness failures use stable `readiness.status` values, including
`daemon_down`, `bad_status`, `invalid_response`, `model_missing`, and
`warmup_failed`. `--verify` and `--warm` exit non-zero when readiness fails.

## harn models recommend

Recommend a starter model from the local hardware snapshot and configured cloud
credentials. The command prefers an Ollama model when no cloud provider key is
set, and emits the selected model plus the one-line rationale.

```bash
harn models recommend
harn models recommend --json
```

The JSON output includes the hardware snapshot used for the lookup: available
RAM, GPU/MPS acceleration, and free disk space.

## harn connect

Authorize connector providers and store local connector secrets in the
workspace keyring namespace.

```bash
harn connect github \
  --app-slug my-harn-app \
  --app-id 12345 \
  --private-key-file app.pem
harn connect slack \
  --client-id "$SLACK_CLIENT_ID" \
  --client-secret "$SLACK_CLIENT_SECRET" \
  --scope "app_mentions:read chat:write"
harn connect linear \
  --client-id "$LINEAR_CLIENT_ID" \
  --client-secret "$LINEAR_CLIENT_SECRET"
harn connect notion \
  --client-id "$NOTION_CLIENT_ID" \
  --client-secret "$NOTION_CLIENT_SECRET"
harn connect generic acme https://mcp.example.com/mcp
harn connect --generic acme https://mcp.example.com/mcp
harn connect acme
harn connect --list
harn connect --refresh notion
harn connect --revoke slack
```

OAuth provider commands use a loopback callback bound to `127.0.0.1`. The
default redirect URI uses port `0`, so Harn selects a random free localhost
port and sends that exact URI in the authorization request. PKCE S256 is always
enabled. Generic OAuth discovers protected-resource and authorization-server
metadata when explicit endpoints are not supplied, attempts dynamic client
registration when available, and sends the `resource` parameter to both the
authorization and token endpoints.

`harn connect <provider>` is available for providers registered in the nearest
`harn.toml` `[[providers]]` table with `oauth = { ... }` metadata. CLI flags
such as `--client-id`, `--scope`, `--auth-url`, and `--token-url` override the
manifest metadata for that run.

Stored OAuth tokens are written under connector-friendly secret ids:

- `<provider>/access-token`
- `<provider>/refresh-token` when the provider returns one
- `<provider>/oauth-token` for the full local refresh metadata

`harn connect --list` reads a small keyring index and shows token expiration
and last-used metadata when known. `--refresh <provider>` forces a refresh-token
grant. `--revoke <provider>` removes the local OAuth token, access token,
refresh token, and index entry.

Provider-specific OAuth flags:

| Flag | Description |
|---|---|
| `--client-id <id>` | Pre-registered OAuth client id |
| `--client-secret <secret>` | OAuth client secret |
| `--scope <scopes>` | Requested scope string |
| `--resource <resource>` | Override the OAuth resource indicator |
| `--auth-url <url>` | Override the authorization endpoint |
| `--token-url <url>` | Override the token endpoint |
| `--token-auth-method <method>` | `none`, `client_secret_post`, or `client_secret_basic` |
| `--redirect-uri <uri>` | Override the loopback callback URI |
| `--no-open` | Print the authorization URL instead of opening a browser |

The GitHub command captures GitHub App installation metadata. If `--app-id` and
`--private-key-file` are supplied, it stores the private key as
`github/app-<app-id>/private-key`. If `--webhook-secret` or
`--webhook-secret-file` is supplied, it stores `github/webhook-secret`.

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

## harn portal

Launch the local Harn observability portal for persisted runs.

```bash
harn portal
harn portal --dir runs/archive
harn portal --manifest examples/personas/harn.toml --persona-state-dir .harn/personas
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

Evaluate a persisted workflow run record as a regression fixture, or
diff a `.harn.prompt` template across a fleet of models with `harn eval
prompt`.

```bash
harn eval .harn-runs/<run>.json
harn eval .harn-runs/<run>.json --compare baseline.json
harn eval .harn-runs/
harn eval evals/regression.json
harn eval harn.eval.toml
harn eval evals/clarifying-question.json
harn eval --llm-mock fixtures.jsonl --structural-experiment doubled_prompt pipeline.harn
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5,gemini-2.5-pro,qwen3.5,ollama:qwen3.5 \
    --bindings examples/coding-agent-system.bindings.json
```

`harn eval` accepts five inputs:

- a single run record JSON file
- a directory of run record JSON files
- an eval suite manifest JSON file with grouped cases and optional baseline comparisons
- an eval-pack v1 TOML/JSON manifest such as `harn.eval.toml`
- a standalone persona eval ladder manifest

Run eval packs declared by a package manifest with:

```bash
harn test package --evals
```

Package eval discovery uses `[package].evals = ["evals/webhooks.toml"]` when
present, otherwise it falls back to `harn.eval.toml` in the package root.

Clarifying-question evals use an explicit fixture with
`"eval_kind": "clarifying_question"`. The fixture checks persisted
`ask_user(...)` prompts captured in the run record and can enforce a
single minimal question via `required_terms`, `forbidden_terms`, and
question-count bounds.

When `path` is a `.harn` pipeline file, `--structural-experiment <spec>` runs
the pipeline twice in isolated temp run directories: once as the baseline and
once with `HARN_STRUCTURAL_EXPERIMENT=<spec>`. The CLI then evaluates both run
sets against their embedded replay fixtures and prints a paired A/B summary.
Use `--llm-mock <fixture.jsonl>` to keep the two runs deterministic.

### harn eval prompt

Render a single `.harn.prompt` template against every model in a fleet,
optionally execute it against each, and surface the resulting wire
envelopes side-by-side. This is the cross-model diff renderer that
exists so capability-adapted variants do not rot silently as the model
mix evolves (see [Capability-aware prompts](./prompt-templating.md#the-llm-scope)).

```bash
# Render-only: produces side-by-side rendered envelopes per model.
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5,gemini-2.5-pro,qwen3.5,ollama:qwen3.5

# Run mode: also calls each model and collects the response.
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5 \
    --mode run

# Judge mode: scores cross-model equivalence with an LLM judge.
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5,gemini-2.5-pro \
    --mode judge \
    --judge-model claude-opus-4-7

# Use a named fleet from `[eval.fleets.<name>]` in harn.toml.
harn eval prompt prompts/agent.harn.prompt --fleet-name frontier --output html -o report.html
```

| Flag | Description |
|---|---|
| `--fleet <list>` | Comma-separated model selectors (alias or `provider:model`). Repeatable. |
| `--fleet-name <name>` | Named fleet from `[eval.fleets.<name>]` in `harn.toml`. |
| `--bindings <path>` | JSON file injected into the template scope. Top-level value must be an object. |
| `--mode <render\|run\|judge>` | Default `render`. |
| `--output <terminal\|json\|html>` | Default `terminal`. |
| `-o, --out-file <path>` | Write `--output` payload to a file instead of stdout. |
| `--max-concurrent <N>` | Cap concurrent provider invocations in `run` / `judge` modes (default 4). |
| `--judge-template <path>` | Override the built-in judge `.harn.prompt`. |
| `--judge-model <selector>` | Judge model (default `claude-opus-4-7`). |
| `--max-tokens <N>` | Cap on completion tokens for `run` / `judge` calls (default 1024). |
| `--fail-on-unauthorized` | Treat unauthenticated providers as errors instead of skipping. |

Fleets can be declared in `harn.toml` and reused:

```toml
[eval.fleets.frontier]
models = ["claude-opus-4-7", "gpt-5", "gemini-2.5-pro"]

[eval.fleets.local]
models = ["ollama:qwen3.5"]
```

Render mode never calls a model — it only pushes the LLM render context
per fleet member and resolves the template so authors can confirm that
`{{ if llm.capabilities.* }}` branches produce the intended wire
envelope on each profile. Run mode synthesizes a thin Harn driver and
routes through the existing `llm_call` infrastructure, so credentials,
provider catalog, and `HARN_LLM_PROVIDER=mock` work exactly as in
`harn run`. Judge mode is run mode plus a final LLM-as-judge call that
asks for a one-line JSON verdict on cross-model equivalence.

## harn merge-captain

Run, audit, or exercise Merge Captain fixtures.

```bash
harn merge-captain run --backend replay examples/personas/merge_captain/transcripts/green_pr.jsonl --once
harn merge-captain audit examples/personas/merge_captain/transcripts/green_pr.jsonl --golden examples/personas/merge_captain/goldens/green_pr.json
harn merge-captain ladder personas/merge_captain/harn.eval.toml --format json --report-out .harn-runs/merge-captain-ladder/report.json
harn merge-captain iterate examples/personas/merge_captain/iterations/smoke.toml --format json
harn merge-captain iterate --diff .harn-runs/merge-captain-iterations/baseline .harn-runs/merge-captain-iterations/candidate
```

`harn merge-captain ladder` runs every configured model route and timeout
tier against the same backend fixture. It writes per-tier JSONL transcripts,
receipts, and summaries, and emits an aggregate report with the first correct
tier plus degraded or looping tiers.

`harn merge-captain iterate` runs the next layer up: scenario × variant sweeps
for agent-led prompt/package iteration. Each variant can carry model route,
timeout tier, Harn package revision, and prompt-asset revision metadata. The
iteration directory is self-contained: it stores copied replay fixtures or
materialized mock playgrounds, per-run JSONL transcripts, receipts, summaries,
`summary.json`, and `summary.md`. The aggregate table ranks variants by
transcript-drift score, then cost. `--diff A B` compares two iteration
directories or summary JSON files and marks scenario/variant cells as improved,
regressed, unchanged, or missing.

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
  --pump-max-outstanding 64 \
  --log-format json \
  --role single-tenant

# Scrape Prometheus metrics from the live listener.
curl http://127.0.0.1:8080/metrics

# Inspect the running orchestrator state, trigger flow-control state, and recent dispatches.
harn orchestrator inspect --state-dir .harn/orchestrator

# Inject a synthetic TriggerEvent to exercise a specific binding.
harn orchestrator fire <trigger-id> --payload event.json

# Replay a historical event through the dispatcher.
harn orchestrator replay <event-id>

# Run replay determinism oracle fixtures.
harn orchestrator replay-oracle

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

# Generate and run a cloud deploy bundle.
harn orchestrator deploy --provider fly --manifest ./harn.toml --build --dry-run
```

`harn orchestrator inspect/fire/replay/dlq/queue` are offline
operations — they read the state snapshot + event log directly. To
operate against a live `harn orchestrator serve`, use the same state
directory. Environment variables `HARN_ORCHESTRATOR_MANIFEST`,
`HARN_ORCHESTRATOR_LISTEN`, `HARN_ORCHESTRATOR_STATE_DIR`,
`HARN_ORCHESTRATOR_API_KEYS`, and `HARN_ORCHESTRATOR_HMAC_SECRET`
configure the serve entry point for container deployments.
`harn orchestrator deploy` accepts `--provider render|fly|railway`,
validates the manifest with the orchestrator runtime, writes provider files
under `deploy/<provider>/`, optionally builds/pushes `--image` with
`--build`, and syncs locally available secrets unless `--no-secret-sync` is
set.

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

# Capture a human replay correction for teaching/policy feedback.
harn trigger replay <event-id> --steer-from outcome --to-decision '{"status":"skipped"}' --reason "human corrected routing" --applied-by alice --scope this_persona

# Preview a filtered bulk replay without dispatching anything.
harn trigger replay --where "event.payload.tenant == 'acme' AND attempt.status == 'failed'" --dry-run

# Replay matching records with progress output and throttling.
harn trigger replay --where "attempt.failed_at > '2026-04-18'" --progress --rate-limit 4
```

Sets `HARN_REPLAY=1` during dispatch so nondeterminism in handlers
can fall back to recorded values when the handler cooperates.
`--steer-from` records the human's injected decision as a typed correction
after a successful single-event replay. The step selector accepts `event`,
`outcome`, or an action-graph node id. `this_persona` and `all` correction
scopes tighten derived capability policy for the affected actor until the
matching correction records no longer apply.

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

## harn flow replay-audit

Audit shipped Flow slices against the current `@retroactive` predicate hashes.
Historical slices remain append-only: drift is advisory unless
`--fail-on-drift` is set.
The `--store` path must already exist; the audit command does not create an
empty Flow store.

```bash
harn flow replay-audit --since 2026-04-26
harn flow replay-audit --since 2026-04-26T12:00:00Z --json
harn flow replay-audit --store .harn/flow.sqlite --root . --target-dir crates/harn-vm --since 2026-04-26 --fail-on-drift
```

| Flag | Description |
|---|---|
| `--since <date>` | Include shipped derived slices created at or after an RFC3339 timestamp, unix timestamp, or `YYYY-MM-DD` |
| `--store <path>` | SQLite Flow store to audit (default: `.harn/flow.sqlite`) |
| `--root <path>` | Repository root used for `invariants.harn` discovery |
| `--target-dir <path>` | Directory whose effective current predicate set is audited |
| `--fail-on-drift` | Exit non-zero when advisory drift is found |
| `--json` | Emit the replay-audit report as JSON |

## harn trace import

Convert a third-party eval trace into a standard `--llm-mock` fixture.

```bash
harn trace import \
  --trace-file traces/generic.jsonl \
  --trace-id trace_123 \
  --output fixtures/imported.jsonl
```

The source file is JSONL. Each line should contain at least
`{prompt, response}` and may also include `tool_calls`, `model`,
`provider`, token counts, and `trace_id`. The generated fixture can be
used directly with `harn run --llm-mock ...`, `harn eval
--llm-mock ...`, or `harn test --determinism`.

## harn crystallize

Mine repeated traces into a reviewable deterministic workflow candidate.

```bash
harn crystallize \
  --from fixtures/crystallize/version-bump \
  --shadow-from fixtures/crystallize/version-bump-holdout \
  --out workflows/version_bump.harn \
  --report reports/version_bump.crystallize.json \
  --eval-pack evals/version_bump.toml \
  --min-examples 5 \
  --workflow-name version_bump
```

The input directory may contain crystallization trace JSON files or persisted
Harn workflow run records. The report preserves source trace hashes,
parameters, side effects, approval points, capability and secret requirements,
shadow-mode pass/fail details, promotion metadata, and cost/token savings.
Pass `--shadow-from <TRACE_DIR>` to add future/holdout traces to the shadow
comparison without using them for mining. Candidates with divergent side
effects or replay-oracle receipt drift are rejected instead of promoted.

Pass `--bundle <DIR>` to also emit a portable
`harn.crystallization.candidate.bundle` directory (`candidate.json`,
`workflow.harn`, `report.json`, `harn.eval.toml`, redacted `fixtures/`)
that Harn Cloud or any other importer can consume without bespoke glue:

```bash
harn crystallize \
  --from fixtures/crystallize/version-bump \
  --out workflows/version_bump.harn \
  --report reports/version_bump.crystallize.json \
  --eval-pack evals/version_bump.toml \
  --bundle bundles/version-bump \
  --bundle-team platform \
  --bundle-repo burin-labs/harn \
  --bundle-risk-level medium \
  --workflow-name version_bump
```

### harn crystallize validate

Smoke-check a bundle on disk: confirms the schema marker and version, that
every referenced file exists, that fixtures are marked redacted, and that
`required_secrets` only lists logical ids (never raw secret values).

```bash
harn crystallize validate bundles/version-bump
```

### harn crystallize shadow

Re-run the deterministic shadow comparison from a bundle's redacted fixtures
in-process, with no live side effects. Returns non-zero on divergence, so it
fits in CI gates.

```bash
harn crystallize shadow bundles/version-bump
```

See [Workflow crystallization](./workflow-crystallization.md) for the trace
schema, bundle layout, and review loop.

The checked-in release/package-maintenance harness can be run with:

```bash
harn crystallize \
  --from crates/harn-vm/tests/fixtures/crystallize_v2_release/mine \
  --shadow-from crates/harn-vm/tests/fixtures/crystallize_v2_release/holdout-pass \
  --out /tmp/release_package_maintenance.harn \
  --report /tmp/release_package_maintenance.report.json \
  --bundle /tmp/release-package-maintenance \
  --min-examples 3 \
  --workflow-name release_package_maintenance \
  --package-name release-workflows \
  --approver release-lead@example.com
```

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

## harn trust-graph verify-chain

Verify the workspace trust graph's hash chain.

```bash
harn trust-graph verify-chain
harn trust-graph verify-chain --json
```

The command reads `trust_graph` and falls back to legacy `trust.graph` logs,
recomputes every `entry_hash`, and checks that each `previous_hash` points to
the prior record.

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

Start a workflow server through one of the outbound transport adapters.

```bash
harn serve a2a agent.harn                  # explicit A2A
harn serve agent.harn                      # legacy A2A shorthand
harn serve --port 3000 agent.harn          # legacy A2A shorthand with custom port
harn serve acp agent.harn                  # ACP session server over stdio
harn serve acp --profile-json /tmp/acp.ndjson agent.harn
harn serve api agent.harn                  # local OpenAPI + SSE Agents API
harn serve api --bind 127.0.0.1:8787 agent.harn
harn serve mcp server.harn                 # exported pub fn -> MCP tools over stdio
harn serve mcp --transport http server.harn
```

`harn serve api` starts the local Harn Agents HTTP API. It serves
`/openapi.json`, health/version/runtime/capability metadata, session and task
CRUD, SSE event streams, local tool-registry inspection, workspace file helpers,
and permission request response endpoints. Prompt execution, cancellation, and
approval decisions are routed through the packaged ACP session runtime so API
clients share the same transcript, EventLog, replay, and host-permission paths
as ACP hosts. Use `--api-key <key>` / `HARN_SERVE_API_KEY`,
`--hmac-secret <secret>` / `HARN_SERVE_HMAC_SECRET`, and the shared `--tls`
flags to protect non-discovery routes.

`harn serve mcp` uses the shared `harn-serve` dispatch core and maps each
exported `pub fn` in the target module to one MCP tool. Tool schemas are
derived from Harn type annotations. With `--transport http`, the server also
supports Streamable HTTP on `--path` plus the legacy SSE compatibility
endpoints `--sse-path` and `--messages-path`.

For scripts that author the MCP surface through the registration
builtins (`mcp_tools(registry)`, `mcp_resource(...)`, `mcp_prompt(...)`)
instead of `pub fn` exports, `harn serve mcp` auto-detects that mode,
runs the script once, and exposes the registered tools / resources /
prompts over either stdio or Streamable HTTP. Pass `--card <PATH_OR_JSON>`
to advertise an MCP v2.1 Server Card.

```bash
harn serve mcp agent.harn                  # auto-detect surface
harn serve mcp agent.harn --card ./card.json
```

`harn serve a2a` uses the shared `harn-serve` dispatch core and exposes each
exported `pub fn` in the target module as an A2A skill. The adapter publishes an
AgentCard at `/.well-known/agent-card.json`, keeps the legacy
`/.well-known/a2a-agent`, `/.well-known/agent.json`, and `/agent/card` aliases,
and supports task send, send-and-wait, streaming/resubscribe, push callback
registration, and cancel propagation. The legacy shorthand `harn serve <file>`
is preserved and rewrites internally to `harn serve a2a <file>`.

`harn serve acp` starts the packaged ACP adapter on stdio for editor and IDE
hosts. It creates ACP sessions, executes the target pipeline for each
`session/prompt`, streams `AgentEvent` values as `session/update`
notifications, and forwards permission prompts through
`session/request_permission`. Use `--api-key <key>` / `HARN_SERVE_API_KEY` or
`--hmac-secret <secret>` / `HARN_SERVE_HMAC_SECRET` to advertise ACP
`authMethods` and require `authenticate` before protected session methods.
Use `--profile` / `HARN_PROFILE=1` to print one categorical timing rollup per
executed `session/prompt`; use `--profile-json <path>` /
`HARN_PROFILE_JSON=<path>` to append per-turn NDJSON records with
`turn`, `session_id`, and `rollup`. File-backed ACP prompts report `vm_setup`
as a profile bucket and reuse a prepared VM baseline while preserving clean
per-turn execution state. `--trace` / `HARN_TRACE=1` enables the LLM trace
summary printed when the stdio server shuts down.

See [MCP and ACP Integration](./mcp-and-acp.md) and
[Outbound workflow server](./harn-serve.md) for protocol details.

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

## Maintainer release scripts

Harn's repo-maintainer release scripts are documented in
[Maintainer release workflow](./maintainer-release.md). They are not part of
the end-user `harn` CLI surface.

## harn add

Add a dependency to `harn.toml`.

```bash
harn add github.com/burin-labs/harn-openapi@v1.2.3
harn add @burin/notion-sdk@1.2.3
harn add @burin/notion-sdk@1.2.3 --registry ./harn-package-index.toml
harn add https://github.com/user/my-lib --alias my-lib --rev v1.2.3
harn add https://github.com/user/my-lib --alias my-lib --branch main
harn add my-lib --git https://github.com/user/my-lib --rev v1.2.3   # legacy form
```

Git dependencies must specify a stable `rev` or an explicit `branch`.
`harn.lock` records the resolved commit and content hash used for
reproducible installs. Registry-name dependencies resolve through the
package index and then write the same git dependency shape as direct
GitHub installs.

## harn install

Install dependencies declared in `harn.toml`, writing or reusing
`harn.lock` and materializing direct plus transitive package
dependencies into `.harn/packages/`.

```bash
harn install
harn install --frozen
harn install --locked --offline
harn install --refetch my-lib
harn install --json
```

`--locked` is a CI-oriented alias for `--frozen`: Harn fails if
`harn.toml` and `harn.lock` disagree. `--offline` also implies locked
behavior and fails instead of fetching when a locked git package is
missing from the shared cache. `--json` emits a structured install
summary suitable for Burin Code and Harn Cloud automation.

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
harn update --all --json
```

`--json` emits a structured update summary, mirroring `harn install`.

## harn remove

Remove one dependency from `harn.toml`, `harn.lock`, and
`.harn/packages/`.

```bash
harn remove my-lib
```

## harn package list

List packages from the current `harn.lock`.

```bash
harn package list
harn package list --json
```

The report includes each locked package's source, materialization status,
integrity status, package version, Harn compatibility range, exported
modules/tools/skills, permissions, and host requirements.

## harn package doctor

Diagnose package install and contract issues.

```bash
harn package doctor
harn package doctor --json
```

Doctor checks for missing or stale lockfiles, missing materialized packages,
content-hash mismatches, declared host capability gaps, and invalid installed
package tool/skill metadata. Publish-readiness checks remain under
`harn package check`, so applications can use doctor without adding package
metadata that only published libraries need.

## harn package search

Search the configured package registry index.

```bash
harn package search notion
harn package search --registry ./harn-package-index.toml --json
```

The registry source comes from `--registry`, `HARN_PACKAGE_REGISTRY`,
`[registry].url` in `harn.toml`, or Harn's default hosted index.

## harn package info

Show registry metadata for one package, optionally at a specific version.

```bash
harn package info @burin/notion-sdk
harn package info @burin/notion-sdk@1.2.3 --json
```

Metadata includes repository, license, Harn compatibility, exported
modules, connector contract compatibility, docs URL, versions, and any
checksum/provenance fields present in the index.

## harn package cache

Inspect and maintain the shared git package cache.

```bash
harn package cache list
harn package cache verify
harn package cache verify --materialized
harn package cache clean
harn package cache clean --all
```

`verify` recomputes cached package content hashes and compares them with
`harn.lock`; `--materialized` also checks `.harn/packages/`. `clean`
removes cache entries not referenced by the current lockfile, while
`--all` clears all cached git package entries.

## harn package outdated

Compare the resolved entries in `harn.lock` against newer registry
versions or upstream branch HEADs.

```bash
harn package outdated
harn package outdated --remote
harn package outdated --json
```

Registry-attributed dependencies (added via `harn add @scope/name@version`)
are checked against the configured registry index; pass `--registry`
to override or `--refresh` to bypass any local cache. Plain git
dependencies report `skipped` unless `--remote` is set, in which case
Harn shells out to `git ls-remote` to detect branch HEAD drift. Path
dependencies always report `skipped` because they live-link.

## harn package audit

Audit `harn.lock` provenance, package compatibility, and supply-chain
integrity in one pass.

```bash
harn package audit
harn package audit --json
harn package audit --skip-materialized
```

The audit verifies that:

- The lock generator/protocol-artifact versions match the running Harn.
- Each entry carries provenance (`package_version`, `manifest_digest`).
- Resolved packages still satisfy their `harn` compatibility range.
- Cached and materialized git packages still hash to the lockfile entry.
- The materialized package's `harn.toml` digest still matches the lock.
- Registry-attributed entries have not been yanked from the index.
- Publishable manifests are not pinned to path dependencies.

Each finding has a stable `code` (`lockfile-stale`, `harn-compat-violation`,
`yanked-registry-version`, etc.) so downstream automation can route
specific failure modes without parsing the human report.

## harn package artifacts

Inspect or verify the published Harn protocol-artifact contract.

```bash
harn package artifacts manifest
harn package artifacts manifest --output vendor/manifest.json
harn package artifacts check vendor/manifest.json
harn package artifacts check vendor/manifest.json --json
```

`manifest` prints (or writes) the protocol-artifact manifest the running
Harn binary would emit — the same JSON that `make gen-protocol-artifacts`
ships under `spec/protocol-artifacts/manifest.json`. `check` compares a
vendored copy of that manifest against the running Harn and reports any
drift. Hosts that vendor Harn protocol bindings (Burin Code, Harn Cloud,
custom integrators) call `check` from CI to detect when a Harn bump
would require regenerating their bindings.

## harn version

Show version information.

```bash
harn version
```
