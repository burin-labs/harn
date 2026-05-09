# Portable workflow bundles

A workflow bundle is Harn's local-first artifact for durable engineering
automations. It is designed to run on a trusted laptop under Burin/Harn and to
remain importable into Harn Cloud later without changing the workflow's durable
identity, graph, policy, or replay metadata.

The canonical on-disk format is JSON. The current schema version is `1`.

```bash
harn workflow validate --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --json
harn workflow preview --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --json
harn workflow preview --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --mermaid
harn workflow run --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --json
```

## Contract

Top-level bundle fields:

| Field | Purpose |
|---|---|
| `schema_version` | Bundle schema version. Must be `1`. |
| `id` / `version` | Stable bundle identity for hosts and importers. |
| `triggers` | Declarations for GitHub, cron, delay, manual, webhook, or MCP wakeups. |
| `workflow` | A normalized Harn `WorkflowGraph` with stable node ids. |
| `prompt_capsules` | Self-contained continuation prompts keyed by capsule id. |
| `policy` | Autonomy tier, tool policy, approval, retry, and catchup behavior. |
| `connectors` | Provider ids, scopes, and setup/status requirements. |
| `environment` | Repo setup profile, worktree policy, and command gates. |
| `receipts` | Replay metadata such as run id, event ids, workflow version, and graph digest. |

Trigger kinds:

| Kind | Required fields |
|---|---|
| `github` | `provider: "github"` and one or more `events`. |
| `cron` | `schedule`. |
| `delay` | `delay`. |
| `manual` | No additional fields. |
| `webhook` | `webhook_path`. |
| `mcp` | `mcp_tool`. |

`harn workflow validate` checks schema version, stable workflow/node ids,
workflow graph validity, trigger references, prompt capsule references, policy
values, connector identity, and environment worktree policy. The validation
report includes a stable `graph_digest` over canonical graph JSON so receipts
and replay runs can pin the exact workflow graph.

## Preview

`harn workflow preview --json` emits the contract Burin GUI/TUI surfaces need
before committing autonomous resources:

- bundle and workflow identity
- graph digest
- validation diagnostics
- trigger declarations
- connector requirements
- environment requirements
- normalized `graph.nodes` for triggers, actions, agents/subagents, waits,
  approvals, connector calls, notifications, catchup/DLQ branches, and terminal
  states
- normalized `graph.edges` connecting connector bindings, trigger dispatch,
  workflow control flow, catchup, DLQ, and terminal outcomes
- node-scoped `graph.diagnostics` so hosts can annotate the exact workflow node
  instead of showing opaque bundle errors
- `graph.editable_fields` JSON pointers for trigger config, prompt capsules,
  model/tool/approval/retry policy, catchup policy, and connector binding
  surfaces
- `graph.mermaid` plus the top-level `mermaid` string for a low-cost debug
  rendering

JSON is the product contract. `--mermaid` prints only the Mermaid view for
quick debugging and docs snippets; hosts should use `--json` for editing and
validation.

## Local run receipts

`harn workflow run --bundle <path> --json` materializes a deterministic local
receipt for the current bundle. The MVP runner walks the reachable graph from
the entry node, records completed node receipts, attaches trigger/event ids, and
emits the connector, policy, environment, workflow version, and graph digest
needed for replay.

The command does not call cloud services or run mutating tools by itself. Hosts
remain responsible for approval UX, concrete file mutations, and notifications;
Harn owns the portable contract, graph digest, deterministic receipt shape, and
replay metadata.

Use `--event-id` and `--trigger-id` to pin a replayed event:

```bash
harn workflow run \
  --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json \
  --trigger-id github-pr-updated \
  --event-id github:event:42 \
  --json
```

The same bundle and replayed event produce the same receipt bytes, which lets
Burin compare local executions and later cloud imports against one stable
artifact.
