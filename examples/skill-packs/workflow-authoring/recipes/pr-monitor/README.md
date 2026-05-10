# Recipe: PR monitor

Watch a pull request, wait for the deploy that follows the merge, query the
deploy logs, and notify the requester. Steel-thread #1 from issue #1407.

## Shape

| Element | Value |
|---|---|
| Triggers | `github` on `pull_request.opened` + `pull_request.synchronize`; `delay` (`PT10M`) for the log check |
| Workflow | `ingest` → `wait_for_deploy` → `query_logs` → `notify` |
| Capsule | `query-logs` continues from the delay trigger with a self-contained prompt |
| Policy | `act_with_approval`, exponential retry (2), `latest` catchup |
| Connector | `github` (pull_requests:read, checks:read), setup + status required |
| Environment | `host_managed` worktree, `make test` gate |

## Run it

```bash
harn workflow validate --bundle examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json --json
harn workflow preview  --bundle examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json --mermaid
harn workflow run      --bundle examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json --json
```

## Why these choices

- The `delay` trigger pinned to `query_logs` is what makes this a "wake later"
  workflow — without it the host would have to invent its own scheduler.
- `act_with_approval` keeps human-in-the-loop on the notify step until a host
  is comfortable promoting the workflow to `act_auto`.
- `catchup.mode = "latest"` means a long-paused supervisor only resumes the
  most recent deploy event instead of replaying every past PR sync.
