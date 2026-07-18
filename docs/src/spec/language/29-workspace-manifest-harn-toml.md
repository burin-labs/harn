<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Workspace manifest (`harn.toml`)

Harn projects declare a workspace manifest at the project root named
`harn.toml`. Tooling walks upward from a target `.harn` file looking
for the nearest ancestor manifest and stops at a `.git` boundary so a
stray manifest in a parent project or `$HOME` is never silently picked
up.

### `[check]` — type-checker and preflight

```toml
[check]
host_capabilities_path = "./schemas/host-capabilities.json"
preflight_severity = "warning"          # "error" (default), "warning", "off"
preflight_allow = ["mystery.*", "runtime.task"]

[check.host_capabilities]
project = ["ensure_enriched", "enrich"]
workspace = ["read_text", "write_text"]
```

- `host_capabilities_path` and `[check.host_capabilities]` declare the
  host-call surface that the preflight pass is allowed to assume exists
  at runtime. The CLI flag `--host-capabilities <file>` takes precedence
  for a single invocation. The external file is JSON or TOML with the
  namespaced shape `{ capability: [op, ...], ... }`; nested
  `{ capabilities: { ... } }` wrappers and per-op metadata dictionaries
  are accepted.
- `preflight_severity` downgrades preflight diagnostics to warnings or
  suppresses them entirely. Type-checker and lint diagnostics are
  unaffected — preflight failures are reported under the `preflight`
  category so IDEs and CI filters can route them separately.
- `preflight_allow` suppresses preflight diagnostics tagged with a
  specific host capability. Entries match an exact `capability.operation`
  pair, a `capability.*` wildcard, a bare `capability` name, or a
  blanket `*`.

Preflight capabilities in this section are a **static check surface**
for the Harn type-checker only. They are not the same thing as ACP's
agent/client capability handshake (`agentCapabilities` /
`clientCapabilities`), which is runtime protocol-level negotiation and
lives outside `harn.toml`.

### `[workspace]` — multi-file targets

```toml
[workspace]
pipelines = ["pipelines", "scripts"]
```

`harn check --workspace` resolves each path in `pipelines` relative to
the manifest directory and recursively checks every `.harn` file under
each. Positional targets remain additive. The manifest is discovered by
walking upward from the first positional target (or the current working
directory when none is supplied).

### `[[personas]]` — durable agent role manifests

`[[personas]]` entries define durable agent roles in the package/workspace
manifest. The canonical typed schema and validator live in `harn-modules` so
CLI, host, cloud, and editor consumers share the same contract. Tooling parses,
validates, lists, and inspects the contract. The continuous runtime records
lifecycle controls, schedule and trigger wake receipts, single-writer leases,
budget receipts, and status snapshots in the `persona.runtime.events` EventLog
topic; long-lived hosted wake loops and typed handoff dispatch remain
host/orchestrator work.

Required fields are `name`, `description`, `entry_workflow`, either `tools` or
`capabilities`, `autonomy_tier`, and `receipt_policy`. Optional fields include
`triggers`, `schedules`, `model_policy`, `budget`, `handoffs`, `context_packs`,
`evals`, `owner`, `version`, `package_source`, and `rollout_policy`.

```toml
[[personas]]
name = "merge_captain"
version = "0.1.0"
description = "Owns pull request readiness, CI triage, merge approvals, and receipts."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github", "ci", "linear", "notion", "slack"]
capabilities = ["git.get_diff", "project.test_commands", "process.exec"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened", "github.check_failed"]
schedules = ["*/30 * * * *"]
handoffs = ["review_captain"]
context_packs = ["repo_policy", "release_rules", "flaky_tests"]
evals = ["merge_safety", "regression_triage", "reviewer_quality"]
budget = { daily_usd = 20.0, frontier_escalations = 3 }
```

Validation rejects missing required fields, malformed or unknown
`capability.operation` entries, invalid cron schedules, unknown handoff targets,
unknown budget/model/source/rollout fields, negative budget amounts, and invalid
rollout percentages. `harn persona list` and `harn persona inspect <name>
--json` expose the resolved schema for hosts such as IDEs and cloud runners.
`harn persona check <path>` validates a package or standalone persona manifest
through the same canonical validator. Persona `triggers` install manifest
trigger bindings whose handler kind is `persona`; explicit `[[triggers]]`
entries may also use `handler = "persona://<name>"`.
`harn persona status <name> --json` exposes stable runtime state including
lifecycle state, current assignment, active lease, last run, next scheduled run,
queued work, typed handoff inbox summaries, budget status, value receipts, and
last error. `pause` queues later events, `resume` drains queued events under
leases, and `disable` records later events as dead-lettered.

### `[dependencies]` and `harn.lock` — git-backed package installs

```toml
[dependencies]
sdk = { version = "^1.2", registry_name = "@burin/notion-sdk", package = "notion-sdk-harn" }
harn-openapi = { git = "https://github.com/burin-labs/harn-openapi", tag = "v1.2.3" }
notion-sdk-harn = { git = "https://github.com/burin-labs/notion-sdk-harn", tag = "v1.2.3" }
notion-connector-harn = { git = "https://github.com/burin-labs/notion-connector-harn", tag = "v1.2.3" }
notion = { git = "https://github.com/burin-labs/notion-sdk-harn", tag = "v1.2.3", package = "notion-sdk-harn" }
openapi = { git = "https://github.com/burin-labs/harn-openapi", branch = "main" }
local-fixture = { path = "../fixture-lib" }
```

`harn install` resolves `[dependencies]` into an immutable package generation
under `.harn/package-generations/<generation>/packages/`, then atomically
publishes `.harn/package-current.toml`. Imports like
`import "notion-sdk-harn"` or `import "notion/providers"` resolve through one
leased snapshot of that generation, never through a directory being mutated.
Each generation contains its exact `harn.lock`, `generation.toml`, and
`lease.lock`; readers hold a shared lease for their full operation, and
garbage collection removes an old generation only after an exclusive lease
succeeds.

- The table key is the local import alias.
- `git` accepts HTTPS, SSH, `file://`, local-repo paths, and GitHub-style
  shorthand URLs.
- Git dependencies must specify exactly one of `tag`, `rev`, or `branch`.
- `tag` pins a git tag in the manifest and lockfile.
- `rev` pins a symbolic ref or full commit SHA in the manifest.
- `branch` records a moving ref in the manifest, but `harn.lock` still
  pins one resolved commit for reproducible installs.
- `version` resolves through the configured registry index, selects the
  highest unyanked semver version matching the range, and then clones the
  git `tag`, `rev`, or `branch` recorded for that registry version.
- `registry_name` records the registry-side package name when it differs
  from the local import alias. Omit it for unscoped registry names that
  match the alias.
- `package` documents the upstream package name when the local alias
  differs from the repository name.
- `path` installs a local directory or `.harn` file without using the
  shared git cache. Directory path dependencies are live-linked into the
  published generation's `packages/<alias>` when the platform supports
  symlinks, with a copy fallback for restricted filesystems. This makes sibling-repo
  development ergonomic for layouts such as `~/projects/{app,
  shared-packages,harn-openapi}`: editing `../harn-openapi/lib.harn` is
  immediately visible to imports in the consuming project.

Transitive package dependencies are resolved from installed package
manifests and flattened into the published generation's `packages/`
directory. For example, a connector package can depend on
`notion-sdk-harn`, and that SDK can depend on `harn-openapi` helpers;
`harn install` records all reachable packages in `harn.lock` and
materializes them from a clean cache. Git-installed packages cannot
declare transitive `path` dependencies, because publishable package
installs must not depend on local sibling directories.

`harn add ../harn-openapi` treats an existing local path as a path
dependency and derives the default alias from that package's
`[package].name` in `harn.toml`, falling back to the directory or file
stem. Use `harn add <alias> --path ../repo` for the legacy explicit
alias form, or `harn add <alias> --git ../repo` when a local git checkout
should be pinned by commit instead of live-linked.

`harn tool new <name>` scaffolds a Harn-native tool package with
`[[package.tools]]` metadata, a stable `tools` export, package-local dispatch
tests, API docs, and CI. `harn skill new <name>` scaffolds a SKILL.md
bundle. Tool and skill packages still install through the
same `[dependencies]`, `harn.lock`, and atomic generation mechanism as ordinary
module packages.

### Eval packs

Packages can ship portable eval packs in `harn.eval.toml` or in paths listed
under `[package].evals`:

```toml
[package]
name = "slack-connector"
version = "0.1.0"
evals = ["evals/webhooks.toml"]
```

After `harn install`, eval-pack discovery includes the root package plus
dependency packages in the leased current generation. Installed
package eval packs are inert until a command or root trigger references them;
installing a package does not install that package's own triggers.

An eval pack is a TOML document with `version = 1`, package metadata, fixture
references, rubrics, optional judge calibration, thresholds, cases, and optional
persona timeout ladders:

```toml
version = 1
id = "slack-connector"
name = "Slack connector evals"

[[fixtures]]
id = "url-verification-run"
kind = "run-record"
path = "fixtures/url-verification.run.json"

[[fixtures]]
id = "url-verification-replay"
kind = "replay-fixture"
path = "fixtures/url-verification.replay.json"

[[rubrics]]
id = "normalization"
kind = "deterministic"

[[rubrics.assertions]]
kind = "run-status"
expected = "completed"

[[cases]]
id = "url-verification"
run = "url-verification-run"
fixture = "url-verification-replay"
rubrics = ["normalization"]
severity = "blocking"

[cases.thresholds]
max-latency-ms = 500
max-cost-usd = 0.001
```

Persona timeout ladders run one fixture across a matrix of model routes and
timeout/budget tiers. The local runner currently supports the Merge Captain
persona and emits per-tier JSONL transcripts, receipts, summaries, counts,
state-machine coverage, and first-correct-tier metadata:

```toml
[[ladders]]
id = "merge-captain-green-pr"
persona = "merge_captain"
artifact-root = ".harn-runs/merge-captain-timeout-ladder"

[ladders.backend]
kind = "replay"
path = "../../examples/personas/merge_captain/transcripts/green_pr.jsonl"

[[ladders.model-routes]]
id = "gemma-value"
route = "local/gemma-value"
provider = "llama.cpp"
model = "gemma"
profile = "value"

[[ladders.timeout-tiers]]
id = "balanced"
timeout-ms = 500
max-tool-calls = 4
max-model-calls = 1
```

Fixture `kind` values are portable labels: `run-record`, `replay-fixture`,
`jsonl-trace`, `provider-events`, and `connector-payload`. Local CLI evaluation
executes run-record/replay-fixture cases and deterministic assertions; other
fixture kinds remain importable metadata for hosted runners.

Rubric `kind` values include `deterministic`, `replay`, `budget`, `hitl`,
`side-effect`, and `llm-judge`. LLM judge rubrics declare model selection,
prompt version, tie handling, confidence minimums, and golden examples, but a
blocking `llm-judge` rubric fails local evaluation unless an explicit judge
runner handles it.

Threshold severity controls deployment-gate behavior:

| Severity | Meaning |
|---|---|
| `blocking` | Failure exits non-zero locally and blocks deployment gates |
| `warning` | Failure is reported but does not block |
| `informational` | Failure is reported for comparison and dashboards only |

Run a pack directly with `harn eval harn.eval.toml`, run root and installed
package-declared packs with `harn test package --evals`, or run a ladder
directly with `harn merge-captain ladder <manifest>`.

Harn source may also declare an eval pack directly:

```harn
eval_pack regression "slack-connector" {
  baseline: "fixtures/baseline.run.json"
  fixtures: [{id: "candidate", kind: "run-record", path: "fixtures/candidate.run.json"}]
  rubrics: [{id: "status", kind: "deterministic", assertions: [{kind: "run-status", expected: "completed"}]}]
  cases: [{id: "url-verification", run: "candidate", rubrics: ["status"]}]
}
```

`eval_pack NAME { ... }` binds `NAME` to `eval_pack_manifest({ ... })`. If a
string id is supplied after the name, that string becomes the manifest id; if
the declaration starts with a string id, Harn derives a valid binding name from
the id. Missing `version` defaults to `1`, and missing `id` defaults to the
header id.

Field entries use `field: expression` and are normalized through the same
eval-pack runtime data model as TOML packs. A top-level `baseline` field acts
as a default `compare_to` path for cases that do not specify their own
baseline.

Eval-pack manifests may set `trials = N` to repeat each case evaluation `N`
times. A case may override the manifest default with its own `trials` value.
The runner records every trial under `report.cases[*].trials`, summarizes the
distribution in `report.cases[*].reliability`, and emits generic stats rows in
`report.stats_rows` with `passes`, `fails`, `trials`, `case_fingerprint`, and
`harness_config_fingerprint` fields. `report.stats.macro_pass_at_1` is the
uniform-case-weighted pass-rate mean over decided cases.

Cases are replay/fixture cases by default. A case with `kind: "live-verify"`
is executed against a live workspace instead of a pre-recorded run record. Live
cases declare `task`, `workspace` (or `project`), `verify_command`,
`expected_output_paths`, `required_output_snippets`, and optional
`tool_budgets`. The manifest or case must declare `executor`, a command spec
that is either a shell string, an argv list, or an object with `command` or
`argv`, plus optional `cwd`, `env`, and `timeout_seconds`. Relative command
working directories resolve under the live workspace.

For each live trial, Harn sends the executor a JSON request on stdin:
`{schema, manifest, case, trial, trials}`. `case.workspace` is an absolute
workspace path, and the case object includes the task, verify command, expected
paths, required snippets, tool budgets, metadata, and case fingerprint. The
executor writes a normalized JSON object to stdout. Generic outcome fields are
`verification` (`PASS`, `FAIL`, or `skip`), `verificationExitCode`,
`timedOut`, `wallTimeSeconds`, `costUsd`, `producedPaths`, and
`toolCallSummary`; snake_case aliases are also accepted. Harn then runs the
case `verify_command` in the workspace, checks expected paths and required
snippets, enforces any declared tool budgets against `toolCallSummary`, and
records the merged normalized outcome under `report.cases[*].trials[*]`.
Executor command details are part of the harness fingerprint, while the live
task, workspace/project reference, verify command, expected outputs, required
snippets, and tool budgets are part of the case fingerprint.

Each normalized case carries `case_fingerprint`, a deterministic SHA-256 prefix
over the case contract: task source, expected output references, resolved
rubrics, comparison target, thresholds, severity, and case metadata. The report
also carries `harness_config_fingerprint`, a deterministic hash over the
manifest-level harness configuration such as model, prompt/tool-format,
pipeline revision metadata, package data, and judge configuration. Paired eval
statistics compare rows only when both the case and harness fingerprints are
compatible.

`eval_pack_run(manifest, options?)` appends one durable eval-ledger row per
trial cell keyed by `(suite, model, split, commit, case,
case_fingerprint, harness_config_fingerprint, trial)` to the active event-log
backend, defaulting to the sqlite event log under the manifest `base_dir` /
`HARN_STATE_DIR`. Before running a cell, the runner reuses an exact matching
row and records the skip in `report.run_state`; rows for the same case/trial
with mismatched fingerprints are refused as resume candidates and reported
under `run_state.fingerprint_refusals`. A rerun where every requested cell is
already present reports `run_state.all_skipped = true` and appends a run-state
heartbeat without duplicating trial rows. `options` may set `namespace`,
`suite`, `model`, `commit`, and `branch` for eval-pack resume identity and
provenance. Lower-level ledger builtins also accept `split`, `case`,
`case_fingerprint`, `harness_config_fingerprint`, and `limit` filters:
`eval_ledger_read`, `eval_ledger_append_rows`,
`eval_ledger_prior_commit_rows`, and `eval_ledger_resolve_resume_plan`.

Declarative trigger manifests may bind a trigger directly to an eval pack with
`handler = "eval_pack://<target>"`. A path-like target (`eval_pack://evals/a.toml`
or an absolute path) loads that TOML/JSON pack relative to `harn.toml`; a bare
target resolves by pack `id`, `name`, or file stem through the root package and
installed package eval declarations (`[package].evals` or default
`harn.eval.toml`). Dispatch runs the normalized manifest through the same
`eval_pack_run` path as scripts, so cron ticks inherit trigger dedupe, retry,
DLQ, replay/cancel, budget, and flow-control handling without a separate
scheduler. Optional trigger-local `ledger = { ... }` or `eval_options = { ... }`
fields are passed as the `eval_pack_run` options.

Manifests may declare named partitions with a `split` block:

```toml
[split]
tune = ["case-a", "case-b"]
holdout = ["case-c"]
```

`eval_pack_validate_split(manifest)` rejects duplicate case ids, duplicate
partition entries, overlapping partitions, unknown case ids, and under-covered
splits. `eval_pack_run(manifest)` performs the same validation before running
cases.

An `eval_pack` block may include ordinary Harn statements and one
`summarize { ... }` block. These statements run when the declaration is
executed in script or block position, with the binding name, `id`, `version`,
and all declared field names in scope. When a file has pipelines, top-level
`eval_pack` declarations are preloaded as manifest values without running
their executable body, so importing or running an unrelated pipeline does not
trigger eval side effects.

### Eval statistics stdlib

`std/eval/stats` provides pure, deterministic eval-meter statistics over
generic row dictionaries. A row should carry `passes`, `trials`, `skips`,
`timeouts`, `wallTimeSeconds`, `costUsd`, and `group`; `name` or `case_name`
identifies the case, and `case_fingerprint`/`caseFingerprint` plus
`harness_config_fingerprint`/`harnessConfigFingerprint` may be supplied to
reject non-comparable paired rows. Ledger aliases such as `pass_rate`,
`total_cost_usd`, and `agent_lane_escalated` are accepted when present.

The module exposes:

| Function | Contract |
|---|---|
| `aggregate_trials(name, outcomes, metadata?)` | Collapses trial outcomes into counts, `PASS`/`FAIL`/`FLAKY`/`skip` status, majority, mean/stdev wall time, and cost fields |
| `eval_fingerprint_integrity(rows)` | Returns the exact observed case-fingerprint map and reports whether every raw row has a case and harness fingerprint, every case has one stable fingerprint, and the cohort has exactly one harness generation |
| `bootstrap_mean_ci(values, resamples, alpha, seed)` | Returns `{mean, lo, hi, std, n}` for a seeded bootstrap; resample indices are drawn from the high-order LCG state bits so power-of-two case counts do not collapse the CI |
| `macro_pass_at_1(rows)` | Computes the uniform-case-weighted mean pass rate over decided cases |
| `reliability_breakdown(rows)` | Returns all-pass, flaky, all-fail, and no-decision fractions plus raw case counts |
| `pass_caret_k(rows)` / `pass_at_k(rows)` | Computes strict pass^k over decided cases |
| `paired_case_deltas(baseline, current)` | Pairs decided rows by case and compatible fingerprint, returning current-minus-baseline pass-rate deltas |
| `paired_delta_report(baseline, current, resamples?, seed?)` | Reports paired bootstrap delta and `status`, where `improved` requires CI lower bound `> 0`, `regression` requires current macro below `baseline - sigma`, and all other results are `inconclusive` |
| `regression_gate(baseline, current, k?)` | Fails when current macro pass@1 is below `baseline - k * sigma` |
| `skip_rate(rows)` / `timeout_rate(rows)` | Mean per-row skip and timeout fractions |
| `worst_group(rows)` | Lowest macro pass@1 group |
| `cost_per_solved(rows)` | Total realized cost divided by solved cases, or `nil` when nothing solved |
| `routing_calibration_report(cheap, ladder, frontier)` | Pairs cheap-only, ladder, and frontier rows to report over-escalation, under-escalation, costs, and convergence-at-frontier |

### Package registry index

`harn package search`, `harn package info`, and registry-name
dependencies use a lightweight TOML index. The registry source is chosen
in this order: a command `--registry` flag, `HARN_PACKAGE_REGISTRY`,
`[registry].url` from the nearest manifest, then Harn's default hosted
index URL. Registry URLs may be `https://`, `http://`, `file://`, or a
filesystem path; relative manifest registry paths resolve from the
manifest directory.

Registry package names are either unscoped names such as `acme-lib` or
scoped names such as `@burin/notion-sdk`. Segments must start with an
ASCII alphanumeric character and may then contain ASCII alphanumerics,
`-`, `_`, or `.`. First-party packages should use the `@burin/`
namespace.

Registry entries map discovery names to the existing git-backed package
manager path; they do not introduce a second package install mechanism.
For example, `harn add @burin/notion-sdk@1.2.3` reads the index entry,
writes the equivalent `[dependencies]` git table, updates `harn.lock`,
and publishes the same immutable package generation that a direct GitHub
install would use.

Manifests may also keep a registry dependency semantic:

```toml
[dependencies]
notion-sdk-harn = { version = "^1.2" }
notion = { version = ">=1.2,<2.0", registry_name = "@burin/notion-sdk", package = "notion-sdk-harn" }
```

`harn install` records the selected exact registry version, resolved git tag
when present, commit SHA, and content hash in `harn.lock`. Frozen/offline
installs reuse that lock entry and local git cache without querying the
registry index again.

```toml
[registry]
url = "https://packages.harnlang.com/harn-package-index.toml"
```

The default URL points at the GitHub-Pages-hosted public index in
`burin-labs/harn-packages`. A Harn Cloud deployment can serve a separately
configured mirror at `/index.toml`. Override per-project via `[registry] url =
...` in `harn.toml`, or globally via `HARN_PACKAGE_REGISTRY`.

Registry index format:

```toml
version = 1

[[package]]
name = "@burin/notion-sdk"
description = "Notion SDK package for Harn connectors"
repository = "https://github.com/burin-labs/notion-sdk-harn"
license = "MIT OR Apache-2.0"
harn = ">=0.7,<0.8"
exports = ["client", "schema"]
connector_contract = "v1"
docs_url = "https://docs.harnlang.com/connectors/notion"
checksum = "sha256:..."
provenance = "https://github.com/burin-labs/notion-sdk-harn/releases/tag/v1.2.3"

[[package.version]]
version = "1.2.3"
git = "https://github.com/burin-labs/notion-sdk-harn"
tag = "v1.2.3"
package = "notion-sdk-harn"
checksum = "sha256:..."
provenance = "https://github.com/burin-labs/notion-sdk-harn/releases/tag/v1.2.3"
```

Package-level metadata includes the registry name, version list,
description, repository, license, Harn compatibility range, exported
modules, connector contract compatibility, docs URL, and optional
checksum/provenance fields. Version entries must specify `git` plus
exactly one of `tag`, `rev`, or `branch`; `tag` or `rev` is preferred for
reproducible installs.

Use registry names when developers should discover first-party or
community packages by capability and stable name. Use direct GitHub refs
for local dogfood, private repositories, unreleased commits, or temporary
pins that are not ready for the shared index.

`harn.lock` is a typed TOML file with `version = 4` and one `[[package]]`
entry per dependency. Each git entry records:

- `source`
- `tag`, when the dependency resolved through a git tag
- `rev_request`
- `commit`
- `content_hash`
- `package_version`
- `harn_compat`
- `provenance`
- `manifest_digest`
- `exports`
- `permissions`
- `host_requirements`

`content_hash` is a SHA-256 over the cached package tree. Harn verifies
that hash whenever it reuses a cached package or prepares a package
generation. `manifest_digest` separately hashes the resolved
package manifest so audit and host policy can detect package-surface drift
without re-hashing the full tree. `provenance`, `exports`, `permissions`, and
`host_requirements` mirror the resolved package's declared source,
module/tool/skill surface, and policy requirements for host UI, CI, and
locked-install review.

For CI and production hosts, `harn install --locked --offline` uses only
the committed `harn.lock` plus the local shared cache; it fails when the
manifest and lockfile disagree or when a locked git package is not
already cached. `harn package cache list`, `clean`, and `verify`
inspect, garbage-collect, and recompute content hashes for cached git
packages. `harn package cache verify --materialized` also verifies
the currently published generation against its exact embedded lockfile hashes.
`harn package list` summarizes locked packages, their exported surfaces,
permissions, host requirements, materialization status, and integrity state.
`harn package doctor` diagnoses missing/stale lockfiles, missing materialized
packages, content-hash mismatches, host capability gaps, and invalid installed
tool or skill metadata. Publish-readiness checks stay under
`harn package check`; applications can run doctor without becoming
publishable packages.

### `[exports]` — stable package module entry points

```toml
[exports]
capabilities = "runtime/capabilities.harn"
providers = "runtime/providers.harn"
```

`[exports]` maps logical import suffixes to package-root-relative module
paths. After `harn install`, consumers import them as
`"<package>/<export>"` instead of coupling to the package's internal
directory layout.

Exports are resolved after direct lookup under the leased generation's
`packages/<path>`, so packages can still expose raw file trees when they want
that behavior.

### `[[package.tools]]` and `[[package.skills]]`

Custom tool and skill packages declare their host-facing surface in the
`[package]` table so installs can be reviewed from the manifest and lockfile:

```toml
[package]
name = "acme-tools"
version = "0.1.0"
provenance = "https://github.com/acme/acme-tools/releases/tag/v0.1.0"
permissions = ["tool:read_only"]
host_requirements = ["workspace.read_text"]

[exports]
tools = "lib/tools.harn"

[[package.tools]]
name = "read-note"
module = "lib/tools.harn"
symbol = "tools"
description = "Read a note through the package tool registry."
permissions = ["tool:read_only"]

[package.tools.input_schema]
type = "object"
required = ["path"]

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"

[[package.skills]]
name = "review"
path = "skills/review"
```

Each tool `module` is a package-root-relative Harn module and `symbol` names
the exported registry builder. `input_schema`, `output_schema`, and
`annotations` are TOML tables that must round-trip to the runtime JSON schema
and tool annotation shapes. Each skill `path` is a package-root-relative
directory containing `SKILL.md` with valid front matter. Package-level
permissions and host requirements are merged with per-tool or per-skill values
when Harn writes the lockfile.

### `[asset_roots]` — package-root prompt asset aliases

```toml
[asset_roots]
partials = "src/prompts/partials"
prompts  = "src/prompts"
```

`[asset_roots]` defines named directories under the project root that
prompt assets can address through `@<alias>/<rel>` paths. The `render`
/ `render_prompt` builtins, the `template.render` host capability, and
`{{ include "..." }}` directives all honor:

- **`@/<rel>`** — anchored at the project root (the harn.toml
  directory).
- **`@<alias>/<rel>`** — anchored at the directory `[asset_roots]`
  maps `<alias>` to.

The project root is derived from the *calling file*, so a render call
inside an imported module resolves the same way regardless of who
called it. Both forms reject `..` segments and absolute targets so a
package-rooted asset can never escape the project root.

`harn check` validates that `@`-prefixed asset paths resolve to a
real file at preflight time. `harn contracts bundle` records every
resolved asset under `prompt_assets`. Plain (non-`@`) paths keep their
legacy source-relative resolution unchanged.

Stdlib prompt assets use embedded `std/...harn.prompt` paths, for example
`std/agent/prompts/tool_contract_text.harn.prompt`. They are the default home
for reusable model-facing stdlib prompt prose, render without filesystem I/O,
and report stable `std://...` provenance URIs.

### `[llm]` — packaged provider extensions

```toml
[llm.providers.my_proxy]
base_url = "https://llm.example.com/v1"
chat_endpoint = "/chat/completions"
completion_endpoint = "/completions"
auth_style = "bearer"
auth_env = "MY_PROXY_API_KEY"
cost_per_1k_in = 0.0002
cost_per_1k_out = 0.0006
latency_p50_ms = 900

[llm.aliases]
my-fast = { id = "vendor/model-fast", provider = "my_proxy" }
```

The `[llm]` table accepts the same schema as `providers.toml`
(`providers`, `aliases`, `inference_rules`, `tier_rules`,
`tier_defaults`, `model_defaults`) but scopes it to the current run.

When Harn starts from a file inside a workspace, it merges:

1. built-in defaults,
2. the global provider file (`HARN_PROVIDERS_CONFIG` or
   `~/.config/harn/providers.toml`),
3. the root project's `[llm]` table.

Installed package manifests do not auto-merge runtime extensions such as
`[llm]`, `[capabilities]`, `[[hooks]]`, or `[[triggers]]` into the host
project. Package code is importable; host runtime configuration remains
root-manifest-owned by default.

### Persona and step lifecycle hooks

Persona scripts can install lifecycle hooks without forking the bundled
persona source:

```harn
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  return admin_merge(ctx)
}

@step(name: "admin_merge")
fn admin_merge(ctx) {
  return ctx
}

pipeline default() {
  register_persona_hook("merge_*", "PreStep", { ctx -> nil })
  register_step_hook("merge_captain", "admin_merge", "PostStep", { ctx ->
    {output: ctx.output}
  })
}
```

`register_persona_hook(persona_pattern, event, handler)` matches a
glob-style persona name and fires for matching lifecycle events.
`register_step_hook(persona_pattern, step_name, event, handler)` further
narrows the hook to one statically declared `@step(name: ...)`. `harn
check` rejects literal step-hook targets whose persona pattern matches a
statically declared `@persona` but whose step name is not declared by
that persona.

Accepted event strings are `PreStep`, `PostStep`,
`OnBudgetThreshold(<pct>)`, `OnApprovalRequested`, `OnHandoffEmitted`,
`OnPersonaPaused`, and `OnPersonaResumed`.

The core VM emits normalized hook payloads with `event`, `target`,
`persona`, and `step` fields. `target` is `persona.step` for step events.
`PreStep` handlers return `nil`, `{deny: "reason"}`, or `{args: [...]}`.
`PostStep` handlers return `nil` or `{output: value}`. Tool hooks still
run inside the step body, so low-level tool policy and high-level
persona policy compose in execution order.

### Preset `run_command` tool hooks

The `std/tool_hooks` module ships a catalogue-driven wrapper for
`run_command`-shaped tool handlers. Rules describe shell-command
faux-pas (rewriteable mistakes such as `find . -name '*.py'`, `cargo
build` without `--target-dir`, or denyable irreversible commands like
`git push --force main`). The shipped catalogues (`harn-canon/*`) live
in `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`
and cover the universal deny set plus per-stack rules for `rust`,
`python`, `typescript`/`ts`, `swift`, `sql`, and `harn`.

`tool_rule(config)` validates a rule at construction:

| Field | Type | Required | Notes |
|---|---|---|---|
| `id` | `string` | yes | Convention `<stack>.<tool>.<short>`; unique within the catalogue. |
| `pattern` | `string` regex or `(command, context) -> bool` callable | yes | Predicate. Regex strings have no lookaround; callables run on every dispatch and must be pure. |
| `applies_to` | `list<string>` | no | Per-rule stack scoping. `[]` matches every opt-in. |
| `severity` | `"error" \| "warning" \| "info"` | no, default `"warning"` | Drives audit-side rendering and shipped-mode dispatch. |
| `explanation` | `string` | no | Single sentence; agent paraphrases it back. |
| `references` | `list<string>` | no | Doc / RFC / post-mortem URLs surfaced in the audit envelope. |
| `priority` | `int` | no, default `0` | Sort key during the linear sweep; higher fires first. |
| `rewrite` | `(command, context) -> string \| dict \| nil` | no | Optional rewriter; `nil`/identical returns skip the rewrite but the audit envelope still flows. |

`catalogue(config)` accepts `{id, rules, stack?, version?, source?, priority?}`.
Stackless catalogues survive `tool_hooks_filter`; stack-tagged
catalogues fire only when the caller opts into the matching `stacks`
list. The shipped catalogues advertise `source: "harn-canon"`.

`preset_run_command(config)` returns an `(args) -> result` closure
shaped for `agent_loop`'s tool registry. Recognized keys:

- `stacks` (`list<string>`, default `[]`) — drives both
  `tool_hooks_filter` and registry auto-seed.
- `registry` (`tool_hooks_registry()` value, default
  `tool_hooks_seed_registry(stacks)`) — explicit override.
- `custom_rules` (`list<tool_rule>`, default `[]`) — matched before
  the registry regardless of stack scoping.
- `mode` (`(rule, args, inner) -> result`, default
  `tool_hooks_mode_rewrite_with_audit`) — match-dispatch callable.
- `inner` (`(args) -> result`, default `nil`) — underlying executor.
  When omitted the wrapper returns decision envelopes without
  executing.
- `llm_classifier` (dict, default `nil`) — optional small-model
  classifier consulted when no deterministic rule matched.

Three shipped modes return a uniform decision envelope:

```text
{action: "rewrite" | "deny" | "passthrough",
 command: <string>,
 original_command: <string>,
 rule_id: <string>,
 catalogue_id: <string>,
 severity: <string>,
 explanation: <string>,
 references: [<string>, ...],
 result?: <inner's return>}
```

- `tool_hooks_mode_rewrite_with_audit` invokes the rule's `rewrite`,
  forwards to `inner`, records a `tool_rewrite` lifecycle audit
  entry, and queues a one-turn `tool_rewritten` system reminder via
  `tool_hooks_inject_reminder`. When the rewrite is identical to the
  original command the envelope still flows.
- `tool_hooks_mode_deny_with_explanation` refuses to dispatch, records
  a `tool_denied` lifecycle audit entry, and never invokes `inner`.
- `tool_hooks_mode_passthrough_only_audit` invokes `inner` with the
  original command unchanged and records a `tool_rule_warning`
  lifecycle audit entry.

Custom modes call the same `tool_hooks_emit_audit(kind, payload)` and
`tool_hooks_inject_reminder({tags, body, ttl_turns, ...})` primitives
and return any envelope shape the caller wants — unknown `action`
strings are treated as advisory extensions by replay tooling.

The optional `llm_classifier` runs a small model against any command
that didn't hit a deterministic rule. Verdicts shape:
`{kind: "rewrite" | "deny" | "allow", confidence, rewritten?, explanation?, references?}`.
Verdicts at or above the configured `threshold` (default `0.8`)
dispatch via the verdict's own mode (`rewrite` →
`tool_hooks_mode_rewrite_with_audit`, `deny` →
`tool_hooks_mode_deny_with_explanation`); `allow` and sub-threshold
verdicts fall through to `inner`. Every classifier invocation emits a
`tool_hook_classifier_verdict` audit regardless of outcome. Transport
errors and non-JSON responses degrade gracefully to passthrough.

The user-facing reference is `docs/src/tool-hooks.md`; runnable
recipes live in `docs/src/cookbooks/tool-hooks.md`; the contribution
guide for harn-canon rules is in
`docs/src/contributing/preset-hooks.md`.

### `[lint]` — lint configuration

```toml
[lint]
disabled = ["unused-import"]
require_file_header = false
require_docstrings = false
complexity_threshold = 25
persona_step_allowlist = ["legacy_helper"]
```

- `disabled` silences the listed rules for the whole project.
- `require_file_header` opts into the `require-file-header` rule,
  which checks that each source file begins with a `/** */` HarnDoc
  block whose title matches the filename.
- `require_docstrings` opts into the `missing-harndoc` rule, which
  warns when a public function has no `/** */` doc comment. Off by
  default — out of the box, `pub fn` needs no docs, and editor
  tooling derives a usage example from the type signature. Embedded
  stdlib sources enforce docstrings regardless of this flag.
- `complexity_threshold` overrides the default cyclomatic-complexity
  warning threshold (default **25**, chosen to match Clippy's
  `cognitive_complexity` default). Set lower to tighten, higher to
  loosen. Per-function escapes still go through `@complexity(allow)`.
- `persona_step_allowlist` lists non-stdlib helper functions that
  `@persona` bodies may call directly without a matching `@step`
  declaration.
