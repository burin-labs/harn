# Personas

Personas are durable agent roles. A persona is not a prompt file; it is an
operational service contract that names an entry workflow and binds it to
triggers, schedules, tools, host capabilities, autonomy, budget ceilings,
handoff targets, context packs, eval packs, rollout policy, and receipt
requirements.

The product principle is simple: personas are operational roles with policies,
not just natural-language behavior.

## Manifest shape

Persona v1 is a typed manifest schema owned by `harn-modules`, so hosts such as
`harn-cli`, cloud platforms, and IDE hosts can parse and validate the same contract.
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

## Handoff routes

Personas may emit typed handoff artifacts, but route selection policy stays in
Harn code. `harn.toml` can carry tenant route data as `[[handoff_routes]]`;
Rust loads and validates the table, then `std/handoffs` helpers select a route,
compose the handoff, and persist dispatch records.

```toml
[[handoff_routes]]
id = "merge-receipt"
kind = "merge_receipt"
from = "merge_captain"
route = [
  { target = "review_captain", when = "always" },
  { target = "human:maintainers", when = "budget_exhausted" },
  { target = "a2a://reviewer.example.com/tasks", when = "approval_denied" },
]
```

`target` accepts a persona name, `persona://name`, `human:group`,
`worker://queue`, or `a2a://endpoint`. Built-in predicates are `always`,
`budget_exhausted`, and `approval_denied`; custom predicate names are resolved
by persona code through the `context.predicates` map passed to
`handoff_route_select(...)`. `handoff_dispatch(...)` records the selected route
in the EventLog and accepts optional local dispatcher hooks for tests or
embedded adapters.

Handoff payloads may include `policy_override`, a `CapabilityPolicy` dict such
as `{ tools = ["read_note"], side_effect_level = "read_only" }`. Dispatch
preserves the policy on `handoff.policy_override`, records
`audit.scope.source_handoff`, and the receiving persona/sub-agent adopts that
policy as the top execution-policy frame for the handoff run. This is a
replacement for the target's current session policy during the handoff, not a
merge with it, so route authors should make the override explicit and scoped to
the delegated task.

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
harn persona new incident_triager --template hybrid-classify-then-act
harn persona compile-prompt \
  --prompt "Triage #alerts in Slack: page me, investigate, or ignore." --json
harn persona new --from-prompt \
  "Every four hours, digest what I need to reply to." --name reply_digest
harn persona materialize --blueprint incident_triager.blueprint.json
harn persona materialize --compile-receipt reviewed-prompt-receipt.json
harn persona --manifest /project/harn.toml materialize \
  --compile-receipt reviewed-prompt-receipt.json --activate --json
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

`persona new` renders from the canonical templates embedded in the CLI, stages
the complete package beside its destination, and publishes it only after every
doctor check and the smoke test pass. `--force` backs up the existing package
and restores it if publication fails; validation failures never modify the
destination.

`persona compile-prompt` makes one schema-bound LLM call and returns a closed
compiler receipt without writing files. The receipt records prompt and compact
trigger-catalog digests, call attempts, model/provider, tokens, realized cost,
the validated blueprint, and its deterministic lowering. Schema retries,
validator retries, and repair are disabled; output defaults to 512 tokens and
cannot exceed 1,200. The model cannot author Harn, TOML, paths, tools,
capabilities, budgets, authority, filters, or notification destinations.

`persona new --from-prompt` passes the accepted lowering directly to the same
strict staged transaction as manual scaffolding. Generated personas always use
`autonomy_tier = "suggest"`, require receipts, and install exactly one validated
daemon trigger. `--name` overrides only the model-proposed persona identifier;
it does not widen the blueprint or policy surface.

`persona materialize --blueprint` accepts only a closed JSON blueprint. Harn
validates and lowers its name, goal, template, and one trigger; the CLI applies
that lowering to the same staged transaction used by `persona new`. It never
accepts generated source, TOML, credentials, or policy overrides.
Alternatively, `--compile-receipt` accepts a successful v1 `compile-prompt`
receipt after review. Harn revalidates its blueprint against the current trigger
catalog and requires the freshly derived lowering to exactly match the reviewed
lowering before entering that same transaction. Replay makes no model call;
stale, failed, incomplete, or edited receipts publish nothing. The two input
flags are mutually exclusive.

Adding `--activate` composes that reviewed-receipt transaction with Harn's local
package installer and project activation ledger. It requires an explicit
`--manifest`, resolves a relative output root from that project rather than the
caller's working directory, chooses a deterministic collision-safe dependency
alias, and activates only the generated `suggest` policy. Its versioned JSON
receipt distinguishes materialization, installation, package doctor,
activation, and runtime verification failures. Repeating the same accepted
receipt reuses identical package bytes, package generation, dependency alias,
and activation record. Burin and other hosts own review and approval UX; Harn
owns this apply/install/activation transaction and its receipts.

`--manifest` accepts a `harn.toml` path or a directory containing one. Without
it, Harn walks up from the current directory to the nearest `harn.toml`, stopping
at a `.git` boundary.

The JSON output is stable enough for hosts such as IDEs and cloud runners to
consume. It includes name, version, tools, capabilities, autonomy tier, model
policy, budget, triggers, handoffs, context packs, evals, receipt policy, and
manifest source.

### Installed persona activation

Personas exported by installed packages use qualified IDs such as
`agents/reviewer`. Installation only makes them discoverable; it does not bind
their schedules, triggers, or workflows. Activation is a separate,
project-scoped authority decision:

```bash
harn persona activate agents/reviewer --json
harn persona activate agents/reviewer --autonomy-tier suggest \
  --tool filesystem --no-capabilities --json
harn persona activations --json
harn persona deactivate agents/reviewer --json
```

Activation writes `.harn/personas/activations.json` beside the resolved root
manifest using an exclusive lock and atomic replacement. Each record pins the
installed package content hash, package-generation lock digest, full exported
persona-contract digest, enforceable effective-policy digest, and a typed
receipt. Repeating the same activation is idempotent.

Local path packages intentionally float in `harn.lock`; activation observes and
pins their current package content. A later local edit makes that activation
stale until it is explicitly refreshed.

An activation may inherit or reduce authority that the runtime enforces.
Autonomy can only move down, and repeated `--tool` or `--capability` flags
select subsets of the exported grants. The matching `--no-*` flag selects an
empty set. Budgets, model policy, receipt policy, package permissions, and host
requirements remain inherited contracts rather than activation attenuation
axes; they are covered by the exported-contract drift pin. Deactivation does not
need the package to remain installed, so stale records are removable.
A configured model implies the exported `llm.call` capability; selecting an
empty capability set disables provider calls while retaining that model policy.
The runtime lifecycle `--state-dir` flag does not relocate this project ledger.

Schema-v1 activation records are validated and retained for audit, but they
cannot execute until explicitly reactivated. This fail-closed migration avoids
treating formerly recorded budget or model fields as runtime grants that the
activation layer does not itself enforce.

Runtime extension loading includes root personas plus dependency personas whose
activation still matches the installed package content and exported policy.
Package or policy drift fails closed with a reactivation diagnostic; installing
or inspecting a package alone never registers its triggers. After activation,
canonical root `[[triggers]]` rows that target the activated persona enter the
runtime with their schedule, matching, secret, and provider policy intact;
unrelated package triggers remain inert. The runtime qualifies the projected
trigger ID and persona handler with the dependency alias, so an explicit handler
may address the same persona as `persona://agents/reviewer` without colliding
with another package's exports. Activated personas keep that qualified identity
in trigger bindings and lifecycle state.
The runtime compiles workflow entries, local trigger predicates, and imports
from the exact bytes captured by a successful content-hash check. Imports may
cross into content-hashed dependencies pinned by the activated generation lock;
paths outside that content-pinned package graph are rejected.

## Trigger handlers

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
handler = "persona://merge_captain" # or persona://agents/reviewer after activation
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
- queued work details, typed handoff inbox summaries, value receipts,
  disabled/dead-lettered event count, and last error

Persisted run records can also carry `persona_runtime[]` snapshots so
orchestration records, portal views, and replay oracle fixtures can compare
persona state without scraping transcript prose. Replay oracle traces expose the
same material as `persona_runtime_states[]`; use that bucket for lifecycle,
budget, handoff, and receipt assertions that must remain deterministic.

Leases are single-writer. A persona run acquires one active lease for the
normalized work key and records a conflict instead of processing duplicate work
while the lease is live. Expired leases are recovered by appending a
`persona.lease.expired` event before the next acquisition.

Pause/resume/disable are explicit controls. Paused personas do not drop events:
incoming events are queued with a `queue_then_drain_on_resume` policy. `resume`
sets the state back to idle and drains queued events once under normal lease and
budget checks. Disabled personas record later events as dead-lettered.

Budget checks run before schedule and trigger work records, and actual usage is
recorded after execution. The admission check compares recorded usage plus any
estimate supplied by the caller; current trigger dispatch starts with zero
estimated cost and tokens. Exceeding a known `daily_usd`, `hourly_usd`,
`run_usd`, or `max_tokens` limit appends a structured budget-exhaustion event
with a receipt id. These are runtime accounting and admission controls, not hard
live spend or token caps inside a provider call.

`harn persona supervision tail` projects `persona.runtime.events` into the
hosted supervision feed shape as newline-delimited JSON. It accepts
`--persona <name>` to narrow the multiplexed stream, `--since-event-id <N>` for
strict greater-than cursor replay, `--limit <N>` for cheap poll loops, and
`--follow` to wait for new appends. Each line carries `event_id`, `persona_id`,
`persona_kind`, optional `persona_version`, `actor`, `update_kind`,
`occurred_at`, and `payload`, matching the hosted `persona/update` frame body.

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
- Harn core owns typed handoff artifacts and runtime inbox visibility; coding
  heuristics such as PR triage strategy, repository conventions, and review
  taste belong in a Burin-specific persona package
- backend-specific systems such as Honeycomb and Splunk should be expressed
  through current tool wiring such as MCP rather than invented manifest fields

## Skill vs persona vs workflow

| Concept | What It Is | Main Unit | Executes? |
|---|---|---|---|
| Skill | Reusable instructions, activation metadata, and optional bundled files. | `SKILL.md` bundle or `skill NAME { ... }`. | No, but it can be loaded into an agent turn. |
| Workflow | Deterministic Harn code that performs work. | `.harn` pipeline/workflow entrypoint. | Yes, when run by the VM or orchestrator. |
| Persona | Durable operational role that points at workflows and adds policy. | `[[personas]]` manifest entry. | Yes for runtime wake/control receipts; workflow execution is still host/orchestrator-owned. |

A skill can teach a model how to do a task. A workflow is the executable path.
A persona says which role owns the work, when it should wake up, what it may
touch, how much it may spend, when it must ask, who it can hand off to, and what
receipt/eval trail proves it behaved correctly.
