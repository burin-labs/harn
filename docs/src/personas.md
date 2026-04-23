# Personas

Personas are durable agent roles. A persona is not a prompt file; it is an
operational service contract that names an entry workflow and binds it to
triggers, schedules, tools, host capabilities, autonomy, budget ceilings,
handoff targets, context packs, eval packs, rollout policy, and receipt
requirements.

The product principle is simple: personas are operational roles with policies,
not just natural-language behavior.

## Manifest Shape

Persona v1 lives in `harn.toml` as `[[personas]]` entries. This keeps personas
compatible with package manifests and the existing manifest discovery model.
Runtime scheduling and typed handoff execution are intentionally outside this
first pass; the CLI only parses, validates, lists, and inspects manifests.

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

`harn persona list` and `harn persona inspect` validate the resolved manifest
before printing output. Validation currently checks:

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
harn persona inspect merge_captain
harn persona inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml inspect merge_captain --json
```

`--manifest` accepts a `harn.toml` path or a directory containing one. Without
it, Harn walks up from the current directory to the nearest `harn.toml`, stopping
at a `.git` boundary.

The JSON output is stable enough for hosts such as Harn Cloud and Burin Code to
consume. It includes name, version, tools, capabilities, autonomy tier, model
policy, budget, triggers, handoffs, context packs, evals, receipt policy, and
manifest source.

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

## Current v1 gaps

Persona manifest v1 is a contract surface, not a scheduler. Harn currently
parses, validates, lists, and inspects personas; it does not yet execute them
from `[[personas]]` entries.

That means template packs should stay honest about missing platform scope:

- schedules validate now but are not runtime wakeups yet
- handoffs validate now but are not typed persona-runtime dispatch yet
- backend-specific systems such as Honeycomb and Splunk should be expressed
  through current tool wiring such as MCP rather than invented manifest fields

## Skill Vs Persona Vs Workflow

| Concept | What It Is | Main Unit | Executes? |
|---|---|---|---|
| Skill | Reusable instructions, activation metadata, and optional bundled files. | `SKILL.md` bundle or `skill NAME { ... }`. | No, but it can be loaded into an agent turn. |
| Workflow | Deterministic Harn code that performs work. | `.harn` pipeline/workflow entrypoint. | Yes, when run by the VM or orchestrator. |
| Persona | Durable operational role that points at workflows and adds policy. | `[[personas]]` manifest entry. | Not in v1; it is parsed and validated for later runtime execution. |

A skill can teach a model how to do a task. A workflow is the executable path.
A persona says which role owns the work, when it should wake up, what it may
touch, how much it may spend, when it must ask, who it can hand off to, and what
receipt/eval trail proves it behaved correctly.
