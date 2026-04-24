# Persona template pack v0

This directory is the first concrete template pack for persona manifest v1.
It is intentionally simple:

- `harn.toml` contains the durable role contracts.
- `workflows/` contains tiny deterministic starter workflows.
- `fixtures/` contains sample payloads teams can fork and extend.
- `context-packs/` contains editable policy/runbook notes.
- `evals/` and `runs/` contain smoke fixtures that work without live
  credentials.

These templates are safe by default:

- side-effecting personas use `autonomy_tier = "act_with_approval"`
- every persona carries `runtime.dry_run` in its capability policy
- model routing defaults to a cheap model and escalates only when needed
- secrets live in [secrets.example.env](./secrets.example.env) as placeholders

## What works today

Persona manifest v1 is now inspection, validation, and durable runtime control.
The CLI can record schedule wakes, trigger wakes, leases, pause/resume/disable
controls, budget receipts, and status JSON for these entries. The checked-in
workflows, fixtures, and evals remain starter artifacts for teams to fork, not
a hidden SaaS control plane.

The current templates make gaps explicit instead of inventing more platform
scope:

- `oncall_captain` uses `tools = ["mcp"]` for observability because Honeycomb
  and Splunk are expected to arrive through MCP wiring today.
- `handoffs` are declared in the manifest and modeled in workflow output, but
  persona-runtime handoff dispatch is not wired in v1.
- schedules can be fired and recorded with `harn persona tick`, but
  deployment-specific always-on wake loops still belong to the orchestrator or
  host.

## Layout

- [harn.toml](./harn.toml)
- [workflows](./workflows)
- [fixtures](./fixtures)
- [context-packs](./context-packs)
- [evals](./evals)
- [runs](./runs)
- [secrets.example.env](./secrets.example.env)

## Validate and inspect

```bash
harn persona --manifest examples/personas/harn.toml list
harn persona --manifest examples/personas/harn.toml inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml inspect review_captain --json
harn persona --manifest examples/personas/harn.toml inspect oncall_captain --json
harn persona --manifest examples/personas/harn.toml status merge_captain --json
```

## Smoke checks

```bash
harn persona --manifest examples/personas/harn.toml tick merge_captain --json
harn persona --manifest examples/personas/harn.toml trigger merge_captain \
  --provider github --kind pull_request \
  --metadata repository=burin-labs/harn --metadata number=462 --json
harn run examples/personas/workflows/merge_captain.harn
harn run examples/personas/workflows/review_captain.harn
harn run examples/personas/workflows/oncall_captain.harn
harn eval examples/personas/evals/template_pack_smoke.json
```

## Customize for a team

1. Copy `examples/personas/` into your repo or package.
2. Edit `harn.toml` to rename owners, triggers, schedules, budgets, and
   connectors.
3. Replace the placeholder context packs with your actual repo policy, release
   rules, review rubric, and on-call runbook.
4. Swap the starter workflows for repo-specific workflows once the role is
   clear.
5. Replace the placeholder secret values in `secrets.example.env` with your own
   local or deployment secret wiring.
6. Re-run persona inspection and the smoke eval before widening rollout.

## External dependencies

The manifests declare tools only from the current v1 surface. Teams still need
to wire the backing systems:

| Persona | Expected systems |
|---|---|
| `merge_captain` | GitHub, CI, Linear, Notion, Slack |
| `review_captain` | GitHub, Notion, Slack |
| `oncall_captain` | PagerDuty, Slack, Notion, GitHub, observability via MCP |

Use placeholder or sandbox credentials while iterating. The smoke fixtures in
this directory do not require any live credentials.
