# Workflow authoring quickstart

This guide takes a fresh `harn` install from "I have the binary" to
"I can author, validate, preview, run, and supervise a portable
workflow bundle." Every command and fixture below is checked in CI by
`scripts/check_docs_workflow_quickstart.sh`, so the snippets below
match what you should see locally.

No paid API credentials are required. The deterministic local runner
walks the bundle's reachable graph and emits a receipt without calling
any LLM or hitting any provider.

## Prerequisites

You need the `harn` CLI on `$PATH`:

```bash
harn --version
```

The fixtures referenced below live in this repo at:

- `docs/fixtures/workflow-bundles/quickstart-minimal.bundle.json`
- `docs/fixtures/workflow-bundles/quickstart-agentic.bundle.json`
- `docs/fixtures/connect-demo/harn.toml`

Copy them to your workspace and edit in place.

## 1. Validate a minimal deterministic bundle

A workflow bundle is JSON while authoring. The smallest viable schema v2
bundle has canonical package metadata, identity fields, one trigger, and one
node.

```bash
harn workflow validate \
  --bundle docs/fixtures/workflow-bundles/quickstart-minimal.bundle.json \
  --json
```

You should see:

```json
{
  "valid": true,
  "bundle_id": "quickstart-minimal",
  "workflow_id": "quickstart_minimal_workflow",
  "graph_digest": "sha256:b31130003507343166b74b52792813ae2b56cd1f4a126c11e84c2af06d69c333",
  "errors": [],
  "warnings": []
}
```

The `graph_digest` is a SHA-256 over the canonical workflow graph. The
same bundle always produces the same digest, which is how Burin GUI/TUI
hosts and Harn Cloud later compare two runs against the same workflow
identity.

## 2. Preview the normalized graph

`harn workflow preview --json` emits the contract that hosts use to
render and edit the workflow. It expands the bundle into nodes for
triggers, connectors, catchup branches, DLQ paths, and terminal states
that the bundle file does not enumerate.

```bash
harn workflow preview \
  --bundle docs/fixtures/workflow-bundles/quickstart-minimal.bundle.json \
  --json
```

For quick visual inspection, the same command supports `--mermaid`:

```bash
harn workflow preview \
  --bundle docs/fixtures/workflow-bundles/quickstart-minimal.bundle.json \
  --mermaid
```

```text
flowchart TD
  n_389fbfc4_node_notify["notification: Notify completion"]
  n_7507e151_node_summarize["action: Summarize input"]
  n_8725ce85_terminal_completed["terminal: Completed"]
  n_df3386d3_terminal_failed["terminal: Failed"]
  n_f5062c77_trigger_manual_start["trigger: manual-start"]
  n_389fbfc4_node_notify -->|completed| n_8725ce85_terminal_completed
  n_7507e151_node_summarize --> n_389fbfc4_node_notify
  n_f5062c77_trigger_manual_start -->|dispatch| n_7507e151_node_summarize
```

Use `--json` for editing and validation; the Mermaid view is for
debugging and quickref docs only. See
[Portable workflow bundles](./workflow-bundles.md) for the full set of
fields the JSON view exposes (`graph.nodes`, `graph.edges`,
`graph.editable_fields`, `graph.diagnostics`).

## 3. Run a deterministic local receipt

`harn workflow run` materializes a deterministic local receipt. It
walks the reachable graph from the entry node, records each node's
status, and copies the bundle's policy/connectors/environment. It does
not invoke LLMs, mutate files, or call provider APIs — those are the
host's job.

```bash
harn workflow run \
  --bundle docs/fixtures/workflow-bundles/quickstart-minimal.bundle.json \
  --json
```

The receipt is reproducible: pinning `receipts.run_id` in the bundle
(or passing `--trigger-id` and `--event-id` for replay) gives you
byte-identical output runs across machines.

## 4. Add prompt capsules and connectors

Real workflows mix deterministic actions with agentic steps and
provider connector calls. The agentic fixture below adds a `review`
node of kind `agent`, attaches a prompt capsule keyed to it, and
declares a GitHub connector requirement:

```bash
harn workflow validate \
  --bundle docs/fixtures/workflow-bundles/quickstart-agentic.bundle.json \
  --json

harn workflow run \
  --bundle docs/fixtures/workflow-bundles/quickstart-agentic.bundle.json \
  --json
```

The local run still needs no API credentials — prompt capsules are
self-contained continuation prompts that hosts execute when they
dispatch the agent node. The bundle stays portable and the receipt
stays deterministic; only the live host run involves the LLM.

To replay the agentic bundle as if a specific GitHub event fired it:

```bash
harn workflow run \
  --bundle docs/fixtures/workflow-bundles/quickstart-agentic.bundle.json \
  --trigger-id github-pr-opened \
  --event-id github:event:pr-1 \
  --json
```

## 5. Inspect connector requirements

Bundles declare provider connectors the host must satisfy before
autonomous dispatch. `harn connect status --json` reports installed
connectors and what they need; `harn connect setup-plan --json` emits
the host setup steps for one connector.

The fixture under `docs/fixtures/connect-demo/` is a self-contained
manifest you can run those commands against without OAuth or real
credentials:

```bash
cd docs/fixtures/connect-demo
harn connect status --json
harn connect setup-plan --connector demo --json
```

You will see the demo connector reported as `installed: true` but
`usable: false` with `status: "missing_auth"` — exactly the surface a
host UI consumes when it offers the user a "Connect" button. The
`recovery` block carries the human-readable remediation copy.

For real providers, see:

- [Connector authoring](./connectors/authoring.md) — package layout and
  setup metadata schema.
- [Connector OAuth](./orchestrator/oauth.md) — provider-specific flows
  for GitHub, Linear, Slack, and Notion.
- [Connector parity matrix](./connectors/parity-matrix.md) — which
  providers are wired today.

## 6. Supervise locally with `harn supervisor`

For trigger-driven workflows that need to react to live events
(GitHub webhooks, cron ticks, MCP wakeups), the supervisor keeps a
durable orchestrator process running and exposes pause/resume/fire/
replay controls. The supervisor reads the same `harn.toml` manifest
your trigger packages live under — it does not consume bundle JSON
directly:

```bash
harn supervisor start --config harn.toml --state-dir .harn/orchestrator --json
harn supervisor list   --config harn.toml --state-dir .harn/orchestrator --json
harn supervisor fire   <workflow-id> --payload-json '{}' --json
harn supervisor stop   --config harn.toml --state-dir .harn/orchestrator --json
```

See [Local workflow supervisor](./workflow-supervisor.md) for the
complete host contract, DLQ commands, and recovery flows.

## 7. Failure modes and remediation

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `validate` reports `unknown trigger kind` | typo in `triggers[].kind` | Use one of `github`, `cron`, `delay`, `webhook`, `mcp`, `manual`. See [Portable workflow bundles](./workflow-bundles.md#contract). |
| `validate` reports `node id mismatch` | the map key in `workflow.nodes` does not match its inner `id` | Make the inner `id` equal the key. |
| `run` exits without executing every node | unreachable nodes from the bundle's `entry` | Add `edges` connecting them, or change `entry`. |
| `connect status --json` returns `manifest: null` | no `harn.toml` found in cwd or parents | `cd` into a directory whose `harn.toml` declares the providers you want to inspect. |
| `connect status` shows `missing_auth` for a demo provider | expected — the demo fixture deliberately has no stored credentials | For real providers, run the `setup_command` from `connect setup-plan --json`. |
| Supervisor `start` errors with "address already in use" | an existing supervisor is bound to `127.0.0.1:8080` | Run `harn supervisor stop` first, or pass `--bind 127.0.0.1:0`. |

## Next steps

- Edit one of the quickstart bundles and re-run `harn workflow validate`
  to watch the diagnostics evolve.
- Read [Portable workflow bundles](./workflow-bundles.md) for the full
  schema and host contract.
- For an in-process host that owns approval UX, file mutations, and
  notifications on top of these bundles, look at Burin Code (the
  desktop host) or Harn Cloud (hosted multi-tenant execution). Both
  consume the same bundle JSON without changes.
