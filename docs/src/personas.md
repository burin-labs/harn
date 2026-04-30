# Personas

Personas are durable agent roles. A persona is not a prompt file; it is an
operational service contract that names an entry workflow and binds it to
triggers, schedules, tools, host capabilities, autonomy, budget ceilings,
handoff targets, context packs, eval packs, rollout policy, and receipt
requirements.

The product principle is simple: personas are operational roles with policies,
not just natural-language behavior.

## Manifest Shape

Persona v1 is a typed manifest schema owned by `harn-modules`, so hosts such as
`harn-cli`, Harn Cloud, and Burin Code can parse and validate the same contract.
The usual form lives in `harn.toml` as `[[personas]]` entries, which keeps
personas compatible with package manifests and the existing manifest discovery
model. Standalone persona TOML files can use the same fields at the top level.

The continuous runtime is intentionally small and event-sourced: it records
schedule and trigger wakes, leases, lifecycle controls, budget receipts, and
status snapshots without inventing a hidden hosted scheduler.

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
handoffs = ["review_captain", "human_maintainer"]
context_packs = ["repo_policy", "release_rules", "flaky_tests"]
evals = ["merge_safety", "regression_triage", "reviewer_quality"]
owner = "platform"
budget = { daily_usd = 20.0, frontier_escalations = 3 }
model_policy = { default_model = "gpt-5.4-mini", escalation_model = "gpt-5.4" }
rollout_policy = { mode = "approval_only", percentage = 25 }
package_source = { package = "ops-personas", path = "personas/merge" }
```

`autonomy` is accepted as an alias for `autonomy_tier`, and `receipts` is
accepted as an alias for `receipt_policy` for hosts that present the shorter
service-contract vocabulary.

Required fields:

| Field | Purpose |
|---|---|
| `name` | Stable persona id. |
| `description` | Human-readable operational responsibility. |
| `entry_workflow` | Pipeline or workflow entrypoint to run when the persona executes. |
| `tools` or `capabilities` | Tool/capability policy surface. At least one must be present. |
| `autonomy_tier` | `shadow`, `suggest`, `act_with_approval`, or `act_auto`. |
| `receipt_policy` | `required`, `optional`, or `disabled`. |

Optional fields:

| Field | Purpose |
|---|---|
| `triggers` | Event names such as `github.pr_opened`. |
| `schedules` | Cron expressions for recurring wakes. |
| `model_policy` | Default/escalation/fallback model preferences. |
| `budget` | Cost, token, escalation, and runtime ceilings. |
| `handoffs` | Other persona names this role can hand work to. |
| `context_packs` | Named context bundles needed by the role. |
| `evals` | Eval pack names that measure persona behavior. |
| `owner` | Human or team owner. |
| `version` | Persona contract version. |
| `package_source` | Package/path/git provenance. |
| `rollout_policy` | Rollout mode, percentage, and cohorts. |

## Validation

`harn persona check`, `harn persona list`, and `harn persona inspect` validate
the resolved manifest before printing output. Validation currently checks:

- missing required fields, including `entry_workflow`
- malformed or unknown `capability.operation` entries
- invalid cron schedules
- unknown handoff target names
- unknown persona, budget, model policy, package source, or rollout fields
- negative budget amounts
- invalid rollout percentages

Capability names are checked against Harn's default host capability surface plus
extra operations declared in `[check.host_capabilities]` or
`host_capabilities_path`.

## CLI

```bash
harn persona list
harn persona list --json
harn persona check personas/ship_captain/harn.toml
harn persona check personas/ship_captain/harn.toml --json
harn persona inspect merge_captain
harn persona inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml status merge_captain --json
harn persona --manifest examples/personas/harn.toml tick merge_captain \
  --at 2026-04-24T12:30:00Z --cost-usd 0.02 --tokens 120 --json
harn persona --manifest examples/personas/harn.toml trigger merge_captain \
  --provider github --kind pull_request \
  --metadata repository=burin-labs/harn --metadata number=462 --json
harn persona --manifest examples/personas/harn.toml pause merge_captain
harn persona --manifest examples/personas/harn.toml resume merge_captain
harn persona --manifest examples/personas/harn.toml disable merge_captain
```

`--manifest` accepts a `harn.toml` path or a directory containing one. Without
it, Harn walks up from the current directory to the nearest `harn.toml`, stopping
at a `.git` boundary.

The JSON output is stable enough for hosts such as IDEs and cloud runners to
consume. It includes name, version, tools, capabilities, autonomy tier, model
policy, budget, triggers, handoffs, context packs, evals, receipt policy, and
manifest source.

## Trigger Handlers

Persona trigger names are first-class trigger registrations. A persona with
`triggers = ["github.pr_opened"]` installs a manifest trigger binding for
provider `github`, event kind `pr_opened`, and handler kind `persona`. Dispatch
records a `persona.trigger.received` event plus the normal persona run receipt
in `persona.runtime.events`.

Explicit trigger manifests can also target a persona:

```toml
[[triggers]]
id = "merge-captain-pr-opened"
kind = "webhook"
provider = "github"
match = { events = ["pr_opened"] }
handler = "persona://merge_captain"
```

## Continuous runtime

Persona runtime commands write durable records to the active EventLog topic
`persona.runtime.events` under `--state-dir` (default `.harn/personas`). The
status query replays those records and returns stable JSON with:

- lifecycle state: `inactive`, `starting`, `idle`, `running`, `paused`,
  `draining`, `failed`, or `disabled`
- `last_run` and `next_scheduled_run`
- active lease id, holder, work key, acquisition time, and expiry
- budget limits, spend, token usage, exhaustion reason, and last receipt id
- queued event count, disabled/dead-lettered event count, and last error

Leases are single-writer. A persona run acquires one active lease for the
normalized work key and records a conflict instead of processing duplicate work
while the lease is live. Expired leases are recovered by appending a
`persona.lease.expired` event before the next acquisition.

Pause/resume/disable are explicit controls. Paused personas do not drop events:
incoming events are queued with a `queue_then_drain_on_resume` policy. `resume`
sets the state back to idle and drains queued events once under normal lease and
budget checks. Disabled personas record later events as dead-lettered.

Budget checks run before schedule and trigger work records. Per-persona
`daily_usd`, `hourly_usd`, `run_usd`, and `max_tokens` caps block expensive
work and append a structured budget-exhaustion event with a receipt id.

External trigger metadata is normalized for common continuous-persona sources:
GitHub PRs and check runs, Linear issues, Slack messages, and generic webhooks.
For example, GitHub PR metadata with `repository=burin-labs/harn` and
`number=462` normalizes to the work key `github:burin-labs/harn:pr:462`.

## Template pack

The first checked-in template pack lives under `examples/personas/`. It ships
three starter personas:

- `merge_captain`
- `review_captain`
- `oncall_captain`

The pack is intentionally conservative:

- dry-run and approval-first defaults for side-effecting roles
- cheap default model routing with explicit escalation models
- placeholder secrets only
- checked-in context packs, fixtures, and smoke eval manifests

Treat these as code and policy that your team forks and edits. They are not
opaque hosted behavior.

Flow persona packs live under `personas/`:

- `personas/ship_captain`: Phase 0 shadow-mode PR emitter for Flow slices.
- `personas/fixer`: consumes inert predicate remediation and proposes follow-up
  slices.

Ship Captain can be inspected with:

```bash
harn persona --manifest personas/ship_captain/harn.toml inspect ship_captain --json
```

## Current v1 gaps

Persona manifest v1 is a contract and runtime-control surface, not a managed
cloud scheduler. Harn records durable wakes and receipts from CLI/runtime
commands, but it does not yet run a long-lived hosted persona supervisor from
`[[personas]]` entries by itself.

That means template packs should stay honest about missing platform scope:

- schedule bindings can be fired and recorded, but deployment-specific
  long-running wake loops still belong to the orchestrator/host
- handoffs validate now but are not typed persona-runtime dispatch yet
- backend-specific systems such as Honeycomb and Splunk should be expressed
  through current tool wiring such as MCP rather than invented manifest fields

## Skill Vs Persona Vs Workflow

| Concept | What It Is | Main Unit | Executes? |
|---|---|---|---|
| Skill | Reusable instructions, activation metadata, and optional bundled files. | `SKILL.md` bundle or `skill NAME { ... }`. | No, but it can be loaded into an agent turn. |
| Workflow | Deterministic Harn code that performs work. | `.harn` pipeline/workflow entrypoint. | Yes, when run by the VM or orchestrator. |
| Persona | Durable operational role that points at workflows and adds policy. | `[[personas]]` manifest entry. | Yes for runtime wake/control receipts; workflow execution is still host/orchestrator-owned. |

A skill can teach a model how to do a task. A workflow is the executable path.
A persona says which role owns the work, when it should wake up, what it may
touch, how much it may spend, when it must ask, who it can hand off to, and what
receipt/eval trail proves it behaved correctly.
