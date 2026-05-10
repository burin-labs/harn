# Recipe: PR repair

Monitor a feature branch for failing checks and surface a deterministic
repair attempt under HITL. Steel-thread #2 from issue #1407.

## Shape

| Element | Value |
|---|---|
| Triggers | `github` on `check_suite.completed` + `pull_request.synchronize`; `cron` (`0 * * * *`) safety net |
| Workflow | `ingest` → `open_worktree` → `repo_setup` → `repair_loop` (agent) → `verify` → `approval` → `notify` |
| Capsule | `repair-loop` ties the agent stage to the GitHub failure trigger |
| Policy | `act_with_approval`, `approval_required: [repair_loop, verify]`, exponential retry (3), `latest` catchup |
| Connector | `github` (PR + checks read/write, contents write), setup + status required |
| Environment | `new_worktree`, gated by `make test` and `make lint` |

## Run it

```bash
harn workflow validate --bundle examples/skill-packs/workflow-authoring/recipes/pr-repair/bundle.json --json
harn workflow preview  --bundle examples/skill-packs/workflow-authoring/recipes/pr-repair/bundle.json --mermaid
harn workflow run      --bundle examples/skill-packs/workflow-authoring/recipes/pr-repair/bundle.json --json
```

## Why these choices

- Two triggers — GitHub events and an hourly cron sweep — give defense in
  depth: the cron catches PRs whose webhook delivery dropped.
- `worktree_policy: "new_worktree"` keeps the user's working tree clean while
  the repair agent runs.
- `approval_required` lists the exact node ids the host must escalate, even
  under `act_with_approval`. This is what lets a host promote individual
  steps to `act_auto` later without rewriting the bundle.
- `make test` + `make lint` as gated commands ensure the verify stage runs
  with the same checks a human would use locally.
